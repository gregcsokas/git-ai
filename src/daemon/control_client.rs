use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::protocol::ControlResponse;

pub fn send_request(socket_path: &Path, request_json: &str) -> Result<ControlResponse, String> {
    let stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("failed to connect to daemon: {}", e))?;

    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| format!("failed to set timeout: {}", e))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("failed to set write timeout: {}", e))?;

    let mut writer = std::io::BufWriter::new(&stream);
    writeln!(writer, "{}", request_json).map_err(|e| format!("failed to send request: {}", e))?;
    writer
        .flush()
        .map_err(|e| format!("failed to flush: {}", e))?;

    let reader = std::io::BufReader::new(&stream);
    let line = reader
        .lines()
        .next()
        .ok_or_else(|| "no response from daemon".to_string())?
        .map_err(|e| format!("failed to read response: {}", e))?;

    serde_json::from_str(&line).map_err(|e| format!("invalid response JSON: {}", e))
}

pub fn is_daemon_running(socket_path: &Path) -> bool {
    send_request(socket_path, r#"{"type":"ping"}"#).is_ok()
}
