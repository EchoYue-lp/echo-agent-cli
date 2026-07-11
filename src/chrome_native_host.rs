use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use echo_agent_app_core::browser::chrome::{CHROME_BRIDGE_PROTOCOL, ChromeBridgeEndpoint};
use serde_json::json;

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub fn is_native_host_invocation() -> bool {
    std::env::args()
        .nth(1)
        .is_some_and(|value| value.starts_with("chrome-extension://") && value.ends_with('/'))
}

pub fn run() -> Result<(), String> {
    let endpoint_path = endpoint_path()?;
    let endpoint_bytes = std::fs::read(&endpoint_path).map_err(|error| {
        format!(
            "failed to read Chrome bridge endpoint {}: {error}",
            endpoint_path.display()
        )
    })?;
    let endpoint: ChromeBridgeEndpoint =
        serde_json::from_slice(&endpoint_bytes).map_err(|error| error.to_string())?;
    if endpoint.protocol != CHROME_BRIDGE_PROTOCOL {
        return Err("Chrome bridge protocol version does not match".to_string());
    }
    let origin = std::env::args()
        .nth(1)
        .filter(|value| value.starts_with("chrome-extension://") && value.ends_with('/'))
        .ok_or_else(|| "Chrome did not provide a valid extension origin".to_string())?;
    let mut stream = TcpStream::connect(("127.0.0.1", endpoint.port))
        .map_err(|error| format!("failed to connect to EKO desktop: {error}"))?;
    stream
        .set_nodelay(true)
        .map_err(|error| error.to_string())?;
    let hello = serde_json::to_vec(&json!({
        "protocol": CHROME_BRIDGE_PROTOCOL,
        "token": endpoint.token,
        "origin": origin,
    }))
    .map_err(|error| error.to_string())?;
    stream
        .write_all(&hello)
        .map_err(|error| error.to_string())?;
    stream.write_all(b"\n").map_err(|error| error.to_string())?;

    let mut outbound_stream = stream.try_clone().map_err(|error| error.to_string())?;
    let outbound = std::thread::spawn(move || -> Result<(), String> {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        loop {
            let payload = read_native_message(&mut input)?;
            outbound_stream
                .write_all(&payload)
                .map_err(|error| error.to_string())?;
            outbound_stream
                .write_all(b"\n")
                .map_err(|error| error.to_string())?;
        }
    });

    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    loop {
        let payload = read_bridge_line(&mut stream)?;
        write_native_message(&mut output, &payload)?;
        if outbound.is_finished() {
            return outbound
                .join()
                .map_err(|_| "Chrome native input thread panicked".to_string())?;
        }
    }
}

fn endpoint_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("EKO_CHROME_BRIDGE_ENDPOINT") {
        return Ok(PathBuf::from(path));
    }
    let home = dirs::home_dir().ok_or_else(|| "home directory is unavailable".to_string())?;
    Ok(home
        .join(".echo-agent")
        .join("browser")
        .join("chrome")
        .join("endpoint.json"))
}

fn read_native_message(reader: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut length_bytes = [0_u8; 4];
    reader
        .read_exact(&mut length_bytes)
        .map_err(|error| error.to_string())?;
    let length =
        usize::try_from(u32::from_ne_bytes(length_bytes)).map_err(|error| error.to_string())?;
    if length > MAX_MESSAGE_BYTES {
        return Err("Chrome message exceeds EKO's 1 MiB bridge limit".to_string());
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice::<serde_json::Value>(&payload).map_err(|error| error.to_string())?;
    Ok(payload)
}

fn read_bridge_line(reader: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut payload = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        reader
            .read_exact(&mut byte)
            .map_err(|error| error.to_string())?;
        let value = byte
            .first()
            .copied()
            .ok_or_else(|| "native bridge returned an empty read buffer".to_string())?;
        if value == b'\n' {
            serde_json::from_slice::<serde_json::Value>(&payload)
                .map_err(|error| error.to_string())?;
            return Ok(payload);
        }
        if payload.len() >= MAX_MESSAGE_BYTES {
            return Err("EKO bridge message exceeds 1 MiB".to_string());
        }
        payload.push(value);
    }
}

fn write_native_message(writer: &mut impl Write, payload: &[u8]) -> Result<(), String> {
    let length = u32::try_from(payload.len()).map_err(|error| error.to_string())?;
    writer
        .write_all(&length.to_ne_bytes())
        .map_err(|error| error.to_string())?;
    writer
        .write_all(payload)
        .map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_message_roundtrips_utf8_json() -> Result<(), String> {
        let payload = r#"{"text":"你好"}"#.as_bytes();
        let mut framed = Vec::new();
        write_native_message(&mut framed, payload)?;
        let decoded = read_native_message(&mut framed.as_slice())?;
        assert_eq!(decoded, payload);
        Ok(())
    }
}
