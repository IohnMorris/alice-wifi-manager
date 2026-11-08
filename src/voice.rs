use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const ALICE_IPC_ADDR: &str = "127.0.0.1:9875";
const MAX_TEXT_LEN: usize = 2000;
static VOICE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Best-effort озвучивание через штатный IPC AliceCapability.
///
/// Ошибка озвучивания никогда не должна менять сетевое состояние менеджера.
pub fn speak(text: &str) {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_TEXT_LEN {
        return;
    }

    let text = text.to_string();
    thread::spawn(move || {
        let lock = VOICE_LOCK.get_or_init(|| Mutex::new(()));
        let Ok(_guard) = lock.lock() else {
            return;
        };

        if let Err(error) = send_frame(&text) {
            crate::wpa_ctrl::append_debug_log(&format!(
                "Voice notification unavailable: {}",
                error
            ));
        }
    });
}

fn send_frame(text: &str) -> Result<(), String> {
    let addr: SocketAddr = ALICE_IPC_ADDR
        .parse()
        .map_err(|error| format!("invalid Alice IPC address: {}", error))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|error| format!("connect {} failed: {}", ALICE_IPC_ADDR, error))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| format!("set IPC write timeout failed: {}", error))?;

    let subscribe = rpc_frame(20, &byte_field(1, b"AliceCapability"));
    let vocalize = vocalize_frame(text);
    stream
        .write_all(&subscribe)
        .and_then(|_| stream.write_all(&vocalize))
        .map_err(|error| format!("write vocalize request failed: {}", error))
}

fn vocalize_frame(text: &str) -> Vec<u8> {
    // START_VOCALIZE=18, events=true, TVocalizeDirective(text, Append=1).
    let directive = concat(&[byte_field(1, text.as_bytes()), integer(2, 1)]);
    let payload = concat(&[integer(1, 18), integer(3, 1), byte_field(13, &directive)]);
    rpc_frame(8, &payload)
}

fn rpc_frame(field: u64, payload: &[u8]) -> Vec<u8> {
    let remote = concat(&[
        byte_field(7, b"AliceCapability"),
        byte_field(field, payload),
    ]);
    let envelope = concat(&[byte_field(252, &remote), integer(253, 210)]);
    let mut frame = (envelope.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&envelope);
    frame
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    let size = parts.iter().map(Vec::len).sum();
    let mut output = Vec::with_capacity(size);
    for part in parts {
        output.extend_from_slice(part);
    }
    output
}

fn integer(field: u64, value: u64) -> Vec<u8> {
    varint(field * 8, value)
}

fn byte_field(field: u64, value: &[u8]) -> Vec<u8> {
    let mut output = varint(field * 8 + 2, value.len() as u64);
    output.extend_from_slice(value);
    output
}

fn varint(mut key: u64, mut value: u64) -> Vec<u8> {
    let mut output = Vec::new();
    while key >= 128 {
        output.push(((key & 127) as u8) | 128);
        key >>= 7;
    }
    output.push(key as u8);

    while value >= 128 {
        output.push(((value & 127) as u8) | 128);
        value >>= 7;
    }
    output.push(value as u8);
    output
}
