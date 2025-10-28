/*
 * pet-input-server: src/main.rs
 *
 * This Rust server listens for global keyboard/mouse events and broadcasts them
 * over WebSocket to any connected client (e.g., your desktop pet).
 *
 * Architecture for "No-Lag":
 * 1. `main` (Async Tokio Thread): Runs the WebSocket server.
 * 2. `rdev::listen` (Separate OS Thread): `rdev` spawns its own thread to
 * listen for system inputs. This is the "hot path".
 * 3. `event_callback` (Hot Path): This function is called by `rdev`. It MUST
 * return almost instantly to prevent system-wide input lag.
 * 4. `tokio::sync::broadcast`: A thread-safe "channel" connects the two.
 * 5. `event_callback`'s only job is to translate the event, apply throttling,
 * and `send()` it to the broadcast channel. This is a 1-microsecond operation.
 * 6. The `main` thread listens to the channel and forwards events to all
 * connected WebSocket clients asynchronously.
 */

use futures_util::{sink::SinkExt, stream::StreamExt};
use rdev::{listen, Event, EventType, Key};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::{accept_async, tungstenite::Message};

// --- Protocol Definition ---
// These structs will be serialized into the exact JSON format
// your Python script was using.

#[derive(Serialize, Clone, Debug)]
struct Coords {
    x: f64,
    y: f64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(untagged)] // This lets `value` be EITHER a String OR a Coords object
enum ActionValue {
    String(String),
    Coords(Coords),
}

#[derive(Serialize, Clone, Debug)]
struct Action {
    kind: String,
    value: ActionValue,
}

// --- Mouse Move Throttling ---
// We use a global, thread-safe Mutex to store the last move time.
// This prevents flooding the WebSocket with 1000s of "MouseMove" events.
use once_cell::sync::Lazy;
static LAST_MOUSE_MOVE: Lazy<Mutex<Instant>> = Lazy::new(|| Mutex::new(Instant::now()));
const MOUSE_MOVE_THROTTLE: Duration = Duration::from_millis(16); // ~60fps

/// The "Hot Path" callback. This MUST be fast.
fn event_callback(event: Event, broadcast_tx: &broadcast::Sender<Action>) {
    // Attempt to translate the `rdev` event into our `Action` protocol
    let action = match event.event_type {
        // --- Mouse Move (with Throttling) ---
        EventType::MouseMove { x, y } => {
            let mut last_move = LAST_MOUSE_MOVE.lock().unwrap();
            if last_move.elapsed() >= MOUSE_MOVE_THROTTLE {
                *last_move = Instant::now();
                Some(Action {
                    kind: "MouseMove".to_string(),
                    value: ActionValue::Coords(Coords { x, y }),
                })
            } else {
                None // Throttled, do nothing
            }
        }

        // --- Mouse Clicks ---
        EventType::ButtonPress(button) => Some(Action {
            kind: "MousePress".to_string(),
            value: ActionValue::String(map_button(button)),
        }),
        EventType::ButtonRelease(button) => Some(Action {
            kind: "MouseRelease".to_string(),
            value: ActionValue::String(map_button(button)),
        }),

        // --- Keyboard ---
        EventType::KeyPress(key) => map_key(key).map(|val| Action {
            kind: "KeyboardPress".to_string(),
            value: ActionValue::String(val),
        }),
        EventType::KeyRelease(key) => map_key(key).map(|val| Action {
            kind: "KeyboardRelease".to_string(),
            value: ActionValue::String(val),
        }),

        // We don't care about mouse wheel events
        _ => None,
    };

    // If we have a valid action, send it to the broadcast channel.
    if let Some(act) = action {
        // `send` is very fast. We ignore the error if no clients are connected.
        let _ = broadcast_tx.send(act);
    }
}

/// Main async function: runs the WebSocket server
#[tokio::main]
async fn main() {
    // 1. Create the broadcast channel.
    //    `tx` is the sender, `_rx` is a receiver we ignore (we subscribe later)
    let (broadcast_tx, _rx) = broadcast::channel::<Action>(1024);

    // 2. Spawn a separate OS thread for `rdev` to listen on.
    //    This is critical. `rdev::listen` blocks its thread, so it MUST
    //    not be on the main `tokio` async thread.
    let tx_clone = broadcast_tx.clone();
    std::thread::spawn(move || {
        println!("Input listener thread started. Listening for global input...");
        // Pass the broadcast sender to the callback
        if let Err(error) = listen(move |event| event_callback(event, &tx_clone)) {
            eprintln!("Error listening to input: {:?}", error);
        }
    });

    // 3. Start the WebSocket server on the main async thread
    let addr = "0.0.0.0:8080";
    let listener = TcpListener::bind(addr).await.expect("Failed to bind");
    println!("WebSocket server started on: ws://{}", addr);

    // 4. Accept new connections
    while let Ok((stream, _)) = listener.accept().await {
        // For each new connection, clone the broadcast sender
        let tx = broadcast_tx.clone();
        // And subscribe to *receive* messages from the channel
        let rx = tx.subscribe();

        // Spawn a new async task for *this specific client*
        tokio::spawn(handle_connection(stream, rx));
    }
}

/// Handles a single WebSocket client connection
async fn handle_connection(stream: TcpStream, mut broadcast_rx: broadcast::Receiver<Action>) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("WebSocket handshake error: {}", e);
            return;
        }
    };
    println!("Client connected.");

    // Split the WebSocket into a sender and receiver
    let (mut ws_sender, _ws_receiver) = ws_stream.split();

    // Loop: wait for a new event from the broadcast channel
    while let Ok(action) = broadcast_rx.recv().await {
        // Serialize our `Action` struct to a JSON string
        let msg_str = match serde_json::to_string(&action) {
            Ok(s) => s,
            Err(_) => continue, // Should not happen
        };

        // Send the JSON string to this client
        if ws_sender.send(Message::Text(msg_str)).await.is_err() {
            // Error sending (e.g., client disconnected)
            // Break the loop for this client.
            break;
        }
    }

    println!("Client disconnected.");
}

// --- Key/Button Mapping Utilities ---
// These functions translate `rdev` keys to the string protocol.

/// Translates `rdev::Button` to "Mouse1", "Mouse2", "Mouse3"
fn map_button(button: rdev::Button) -> String {
    match button {
        rdev::Button::Left => "Mouse1".to_string(),
        rdev::Button::Right => "Mouse2".to_string(),
        rdev::Button::Middle => "Mouse3".to_string(),
        // Map other buttons if your pet needs them
        rdev::Button::Unknown(code) => format!("Mouse{}", code),
        // **FIXED:** Removed the unreachable `_` arm
    }
}

// This map normalizes Left/Right variants (e.g., ControlLeft -> Control)
// This is lazy-loaded for efficiency
static KEY_MAP: Lazy<HashMap<Key, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert(Key::Space, "Space");
    m.insert(Key::Alt, "Alt");
    m.insert(Key::AltGr, "Alt");
    m.insert(Key::ControlLeft, "Control");
    m.insert(Key::ControlRight, "Control");
    m.insert(Key::ShiftLeft, "Shift");
    m.insert(Key::ShiftRight, "Shift");
    m.insert(Key::MetaLeft, "Meta"); // "Cmd" on macOS, "Windows" key
    m.insert(Key::MetaRight, "Meta");
    m.insert(Key::Escape, "Escape");
    m.insert(Key::F1, "F1");
    m.insert(Key::F2, "F2");
    m.insert(Key::F3, "F3");
    m.insert(Key::F4, "F4");
    m.insert(Key::F5, "F5");
    m.insert(Key::F6, "F6");
    m.insert(Key::F7, "F7");
    m.insert(Key::F8, "F8");
    m.insert(Key::F9, "F9");
    m.insert(Key::F10, "F10");
    m.insert(Key::F11, "F11");
    m.insert(Key::F12, "F12");
    m.insert(Key::Tab, "Tab");
    m.insert(Key::Return, "Return"); // 'Enter' key
    m.insert(Key::Backspace, "Backspace");
    m.insert(Key::CapsLock, "CapsLock");
    m.insert(Key::Insert, "Insert");
    m.insert(Key::Delete, "Delete");
    m.insert(Key::Home, "Home");
    m.insert(Key::End, "End");
    m.insert(Key::PageUp, "PageUp");
    m.insert(Key::PageDown, "PageDown");
    m.insert(Key::UpArrow, "UpArrow");
    m.insert(Key::DownArrow, "DownArrow");
    m.insert(Key::LeftArrow, "LeftArrow");
    m.insert(Key::RightArrow, "RightArrow");
    m.insert(Key::KeyQ, "KeyQ");
    m.insert(Key::KeyW, "KeyW");
    m.insert(Key::KeyE, "KeyE");
    m.insert(Key::KeyR, "KeyR");
    m.insert(Key::KeyT, "KeyT");
    m.insert(Key::KeyY, "KeyY");
    m.insert(Key::KeyU, "KeyU");
    m.insert(Key::KeyI, "KeyI");
    m.insert(Key::KeyO, "KeyO");
    m.insert(Key::KeyP, "KeyP");
    m.insert(Key::KeyA, "KeyA");
    m.insert(Key::KeyS, "KeyS");
    m.insert(Key::KeyD, "KeyD");
    m.insert(Key::KeyF, "KeyF");
    m.insert(Key::KeyG, "KeyG");
    m.insert(Key::KeyH, "KeyH");
    m.insert(Key::KeyJ, "KeyJ");
    m.insert(Key::KeyK, "KeyK");
    m.insert(Key::KeyL, "KeyL");
    m.insert(Key::KeyZ, "KeyZ");
    m.insert(Key::KeyX, "KeyX");
    m.insert(Key::KeyC, "KeyC");
    m.insert(Key::KeyV, "KeyV");
    m.insert(Key::KeyB, "KeyB"); // **FIXED:** Removed the "T " typo
    m.insert(Key::KeyN, "KeyN");
    m.insert(Key::KeyM, "KeyM");
    m.insert(Key::Num1, "Num1");
    m.insert(Key::Num2, "Num2");
    m.insert(Key::Num3, "Num3");
    m.insert(Key::Num4, "Num4");
    m.insert(Key::Num5, "Num5");
    m.insert(Key::Num6, "Num6");
    m.insert(Key::Num7, "Num7");
    m.insert(Key::Num8, "Num8");
    m.insert(Key::Num9, "Num9");
    m.insert(Key::Num0, "Num0");
    // Numpad keys
    m.insert(Key::Kp0, "Num0");
    m.insert(Key::Kp1, "Num1");
    m.insert(Key::Kp2, "Num2");
    m.insert(Key::Kp3, "Num3");
    m.insert(Key::Kp4, "Num4");
    m.insert(Key::Kp5, "Num5");
    m.insert(Key::Kp6, "Num6");
    m.insert(Key::Kp7, "Num7");
    m.insert(Key::Kp8, "Num8");
    m.insert(Key::Kp9, "Num9");
    m // **FIXED:** Removed the stray "m" from here
});

/// Translates `rdev::Key` to the protocol string (e.g., "KeyA", "Space")
fn map_key(key: rdev::Key) -> Option<String> {
    // Check our map first for special keys
    if let Some(mapped) = KEY_MAP.get(&key) {
        return Some(mapped.to_string());
    }

    // Fallback for keys not in the map (e.g., ';', '=', ',')
    // `rdev` doesn't have good variants for these, so we use `Unknown`
    if let Key::Unknown(code) = key {
        // This part is OS-dependent and less reliable,
        // but it's the best we can do for non-standard keys.
        // On Windows, `code` is a virtual key code.
        // We can add more common ones here if needed.
        match code {
            188 => Some(",".to_string()),
            190 => Some(".".to_string()),
            191 => Some("/".to_string()),
            186 => Some(";".to_string()),
            222 => Some("'".to_string()),
            219 => Some("[".to_string()),
            221 => Some("]".to_string()),
            220 => Some("\\".to_string()),
            189 => Some("-".to_string()),
            187 => Some("=".to_string()),
            _ => None,
        }
    } else {
        None
    }
}