use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::{Read, Write}, net::{TcpStream, ToSocketAddrs}, path::PathBuf, time::{Duration, Instant}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedServer {
    pub name: String,
    pub address: String,
}

#[derive(Clone, Debug, Default)]
pub struct ServerStatus {
    pub online: bool,
    pub version: String,
    pub players: String,
    pub motd: String,
    pub latency_ms: u128,
}

fn path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "SilverDevStudios", "LUNAR")
        .map(|dirs| dirs.config_dir().join("servers.json"))
}

pub fn load() -> Vec<SavedServer> {
    path().and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(servers: &[SavedServer]) -> Result<()> {
    let path = path().context("Could not locate the LUNAR configuration directory")?;
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(servers)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn varint(mut value: i32) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        if value & !0x7f == 0 { bytes.push(value as u8); return bytes; }
        bytes.push(((value & 0x7f) | 0x80) as u8);
        value = ((value as u32) >> 7) as i32;
    }
}

fn read_varint(stream: &mut TcpStream) -> Result<i32> {
    let mut value = 0_i32;
    for index in 0..5 {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte)?;
        value |= ((byte[0] & 0x7f) as i32) << (7 * index);
        if byte[0] & 0x80 == 0 { return Ok(value); }
    }
    bail!("Invalid Minecraft status packet length")
}

fn packet(payload: &[u8]) -> Vec<u8> {
    let mut output = varint(payload.len() as i32);
    output.extend_from_slice(payload);
    output
}

fn read_string(stream: &mut TcpStream) -> Result<String> {
    let length = read_varint(stream)?;
    if !(0..=1_048_576).contains(&length) { bail!("Invalid Minecraft status response size"); }
    let mut bytes = vec![0; length as usize];
    stream.read_exact(&mut bytes)?;
    Ok(String::from_utf8(bytes)?)
}

fn flatten_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(flatten_text).collect::<Vec<_>>().join(""),
        Value::Object(object) => {
            let mut text = object.get("text").and_then(Value::as_str).unwrap_or_default().to_owned();
            if let Some(extra) = object.get("extra").and_then(Value::as_array) {
                text.push_str(&extra.iter().map(flatten_text).collect::<Vec<_>>().join(""));
            }
            text
        }
        _ => String::new(),
    }
}

pub fn ping(server: &SavedServer) -> Result<ServerStatus> {
    let address = if server.address.contains(':') { server.address.clone() } else { format!("{}:25565", server.address) };
    let socket = address.to_socket_addrs()?.next().context("Server address did not resolve")?;
    let started = Instant::now();
    let mut stream = TcpStream::connect_timeout(&socket, Duration::from_secs(4)).context("Connection timed out")?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let host = server.address.split(':').next().unwrap_or(&server.address);
    let mut handshake = vec![0, 0xff, 0xff, 0xff, 0xff, 0x0f];
    handshake.extend(varint(host.len() as i32));
    handshake.extend(host.as_bytes());
    handshake.extend(25565_u16.to_be_bytes());
    handshake.push(1);
    stream.write_all(&packet(&handshake))?;
    stream.write_all(&[1, 0])?;
    let packet_length = read_varint(&mut stream)?;
    if !(1..=1_048_576).contains(&packet_length) { bail!("Invalid Minecraft status response"); }
    let packet_id = read_varint(&mut stream)?;
    if packet_id != 0 { bail!("Unexpected Minecraft status packet"); }
    let response: Value = serde_json::from_str(&read_string(&mut stream)?)?;
    let latency_ms = started.elapsed().as_millis();
    let online = response.pointer("/players/online").and_then(Value::as_u64).unwrap_or(0);
    let max = response.pointer("/players/max").and_then(Value::as_u64).unwrap_or(0);
    Ok(ServerStatus {
        online: true,
        version: response.pointer("/version/name").and_then(Value::as_str).unwrap_or("Unknown").to_owned(),
        players: format!("{online}/{max}"),
        motd: flatten_text(response.get("description").unwrap_or(&Value::Null)),
        latency_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_varints() {
        assert_eq!(varint(0), vec![0]);
        assert_eq!(varint(127), vec![127]);
        assert_eq!(varint(128), vec![128, 1]);
    }

    #[test]
    fn flattens_motd_components() {
        let value = serde_json::json!({"text":"Hello ","extra":[{"text":"world"}]});
        assert_eq!(flatten_text(&value), "Hello world");
    }
}
