/*
 * pet-input-server: src/main.rs
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

// --- NEW ---
use clap::Parser;

// --- Protocol Definition ---
// ... (Structs Action, Coords, ActionValue are all unchanged) ...
#[derive(Serialize, Clone, Debug)]
struct Coords {
    x: f64,
    y: f64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(untagged)]
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
// ... (Lazy static LAST_MOUSE_MOVE is unchanged) ...
use once_cell::sync::Lazy;
static LAST_MOUSE_MOVE: Lazy<Mutex<Instant>> = Lazy::new(|| Mutex::new(Instant::now()));
const MOUSE_MOVE_THROTTLE: Duration = Duration::from_millis(16); // ~60fps


// --- NEW: Command Line Argument Definition ---
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// The port to listen on
    #[arg(short, long, default_value_t = 8080)]
    port: u16,
}
// --- END NEW ---


/// The "Hot Path" callback. This MUST be fast.
// ... (This entire function is unchanged) ...
fn event_callback(event: Event, broadcast_tx: &broadcast::Sender<Action>) {
    let action = match event.event_type {
        EventType::MouseMove { x, y } => {
            let mut last_move = LAST_MOUSE_MOVE.lock().unwrap();
            if last_move.elapsed() >= MOUSE_MOVE_THROTTLE {
                *last_move = Instant::now();
                Some(Action {
                    kind: "MouseMove".to_string(),
                    value: ActionValue::Coords(Coords { x, y }),
                })
            } else {
                None
            }
        }
        EventType::ButtonPress(button) => Some(Action {
            kind: "MousePress".to_string(),
            value: ActionValue::String(map_button(button)),
        }),
        EventType::ButtonRelease(button) => Some(Action {
            kind: "MouseRelease".to_string(),
            value: ActionValue::String(map_button(button)),
        }),
        EventType::KeyPress(key) => map_key(key).map(|val| Action {
            kind: "KeyboardPress".to_string(),
            value: ActionValue::String(val),
        }),
        EventType::KeyRelease(key) => map_key(key).map(|val| Action {
            kind: "KeyboardRelease".to_string(),
            value: ActionValue::String(val),
        }),
        _ => None,
    };

    if let Some(act) = action {
        if act.kind != "MouseMove" {
            println!("Broadcasting action: {:?}", act);
        }
        let _ = broadcast_tx.send(act);
    }
}

/// Main async function: runs the WebSocket server
#[tokio::main]
async fn main() {
    // --- CHANGED ---
    // 1. Parse command-line arguments
    let cli = Cli::parse();
    let port = cli.port;
    // --- END CHANGED ---

    // 2. Create the broadcast channel.
    let (broadcast_tx, _rx) = broadcast::channel::<Action>(1024);

    // 3. Spawn a separate OS thread for `rdev` to listen on.
    let tx_clone = broadcast_tx.clone();
    std::thread::spawn(move || {
        println!("Input listener thread started. Listening for global input...");
        if let Err(error) = listen(move |event| event_callback(event, &tx_clone)) {
            eprintln!("Error listening to input: {:?}", error);
        }
    });

    // --- CHANGED ---
    // 4. Start the WebSocket server on the main async thread
    //    Use the port from the CLI arguments
    let addr = format!("0.0.0.0:{}", port);
    // --- END CHANGED ---

    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");

    // --- CHANGED ---
    // 5. Print the address we are *actually* using
    println!("WebSocket server started on: ws://{}", addr);
    // --- END CHANGED ---

    // 6. Accept new connections
    while let Ok((stream, _)) = listener.accept().await {
        let tx = broadcast_tx.clone();
        let rx = tx.subscribe();
        tokio::spawn(handle_connection(stream, rx));
    }
}

/// Handles a single WebSocket client connection
// ... (This entire function is unchanged) ...
async fn handle_connection(stream: TcpStream, mut broadcast_rx: broadcast::Receiver<Action>) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("WebSocket handshake error: {}", e);
            return;
        }
    };
    println!("Client connected.");

    let (mut ws_sender, _ws_receiver) = ws_stream.split();

    while let Ok(action) = broadcast_rx.recv().await {
        let msg_str = match serde_json::to_string(&action) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if ws_sender.send(Message::Text(msg_str)).await.is_err() {
            break;
        }
    }
    println!("Client disconnected.");
}

// --- Key/Button Mapping Utilities ---
// ... (map_button and map_key functions are unchanged) ...

fn map_button(button: rdev::Button) -> String {
    match button {
        rdev::Button::Left => "Mouse1".to_string(),
        rdev::Button::Right => "Mouse2".to_string(),
        rdev::Button::Middle => "Mouse3".to_string(),
        _ => "Mouse1".to_string(),
    }
}

static KEY_MAP: Lazy<HashMap<Key, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert(Key::Space, "Space");
    m.insert(Key::Alt, "Alt");
    m.insert(Key::AltGr, "Alt");
    m.insert(Key::ControlLeft, "Control");
    m.insert(Key::ControlRight, "Control");
    m.insert(Key::ShiftLeft, "Shift");
    m.insert(Key::ShiftRight, "Shift");
    m.insert(Key::MetaLeft, "Meta");
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
    m.insert(Key::Return, "Return");
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
    m.insert(Key::KeyB, "KeyB");
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
    m
});

fn map_key(key: rdev::Key) -> Option<String> {
    if let Some(mapped) = KEY_MAP.get(&key) {
        return Some(mapped.to_string());
    }
    if let Key::Unknown(code) = key {
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