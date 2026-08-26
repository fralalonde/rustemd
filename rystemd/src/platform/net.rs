//! Unix-socket IPC: the control stream (`systemctl`/`SocketClient` → daemon),
//! the `sd_notify` datagram, and the client-side request helper.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::path::Path;

use serde_json::Value;

/// Bind (and rebind) a stream listener for the control channel. Non-blocking:
/// the event loop drives `accept()` via `poll`, so a blocking accept would
/// stall the whole manager.
pub fn bind_control(path: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path).map_err(|e| e.to_string())?;
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(l)
}

/// Bind (and rebind) the datagram socket services use for `sd_notify`.
/// Non-blocking for the same reason as [`bind_control`].
pub fn bind_notify(path: &Path) -> Result<UnixDatagram, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(path);
    let s = UnixDatagram::bind(path).map_err(|e| e.to_string())?;
    s.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Send one JSON request line to `socket` and return the response `data`.
pub fn request(socket: &Path, req: &Value) -> Result<Value, String> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| format!("Failed to connect to manager at {}: {e}", socket.display()))?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let mut line = serde_json::to_string(req).map_err(|e| e.to_string())?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;

    let mut reader = BufReader::new(stream);
    let mut out = String::new();
    reader
        .read_line(&mut out)
        .map_err(|e| format!("failed to read response: {e}"))?;
    let v: Value = serde_json::from_str(&out).map_err(|e| format!("bad response: {e}"))?;
    if v.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string())
    }
}
