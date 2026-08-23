//! Socket-activation listener types and binding. Compiled only with the
//! `socket` feature; the manager core calls these from its `SocketUnit` path.

use std::net::TcpListener;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixListener;

/// A bound, non-blocking listening socket for a `.socket` unit.
pub enum SocketListener {
    Unix(UnixListener),
    Tcp(TcpListener),
}

impl SocketListener {
    pub fn as_raw_fd(&self) -> RawFd {
        match self {
            SocketListener::Unix(l) => l.as_raw_fd(),
            SocketListener::Tcp(l) => l.as_raw_fd(),
        }
    }
}

/// Bind a `ListenStream=` spec. Mirrors systemd's address grammar: `/path` or
/// `unix:/path` (or `@abstract`) is a unix socket; anything else is TCP
/// (`host:port`, or a bare port bound to 0.0.0.0).
pub fn bind_listen_stream(spec: &str) -> Result<SocketListener, String> {
    if let Some(path) = spec.strip_prefix("unix:") {
        bind_unix(path)
    } else if spec.starts_with('/') {
        bind_unix(spec)
    } else if spec.starts_with('@') {
        // Abstract unix socket namespace (Linux-only) — deferred.
        Err(format!("abstract unix sockets not supported: {spec}"))
    } else {
        bind_tcp(spec)
    }
}

fn bind_unix(path: &str) -> Result<SocketListener, String> {
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path).map_err(|e| format!("bind {path}: {e}"))?;
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(SocketListener::Unix(l))
}

fn bind_tcp(spec: &str) -> Result<SocketListener, String> {
    let addr: std::net::SocketAddr = if let Ok(port) = spec.parse::<u16>() {
        std::net::SocketAddr::from(([0, 0, 0, 0], port))
    } else {
        spec.parse()
            .map_err(|e| format!("bad address `{spec}`: {e}"))?
    };
    let l = TcpListener::bind(addr).map_err(|e| format!("bind {spec}: {e}"))?;
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(SocketListener::Tcp(l))
}
