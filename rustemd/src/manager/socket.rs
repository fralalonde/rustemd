//! Socket-activation listener types and binding. Compiled only with the
//! `socket` feature; the manager core calls these from its `SocketUnit` path.

use std::net::TcpListener;
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

#[cfg(unix)]
pub type SocketId = RawFd;
#[cfg(windows)]
pub type SocketId = usize;

/// A bound, non-blocking listening socket for a `.socket` unit.
pub enum SocketListener {
    #[cfg(unix)]
    Unix(UnixListener),
    Tcp(TcpListener),
}

impl SocketListener {
    pub fn id(&self) -> SocketId {
        match self {
            #[cfg(unix)]
            SocketListener::Unix(listener) => listener.as_raw_fd(),
            #[cfg(unix)]
            SocketListener::Tcp(listener) => listener.as_raw_fd(),
            #[cfg(windows)]
            SocketListener::Tcp(listener) => listener.as_raw_socket() as usize,
        }
    }

    #[cfg(windows)]
    pub fn take_trigger(&self) -> bool {
        match self {
            SocketListener::Tcp(listener) => match listener.accept() {
                Ok((stream, _)) => {
                    drop(stream);
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(_) => false,
            },
        }
    }
}

/// Bind a `ListenStream=` spec. Unix accepts filesystem-domain and TCP
/// listeners. The Windows MVP accepts TCP listeners; named-pipe control IPC
/// is separate from `.socket` units.
pub fn bind_listen_stream(spec: &str) -> Result<SocketListener, String> {
    if let Some(path) = spec.strip_prefix("unix:") {
        bind_unix(path)
    } else if spec.starts_with('/') {
        bind_unix(spec)
    } else if spec.starts_with('@') {
        Err(format!("abstract unix sockets not supported: {spec}"))
    } else {
        bind_tcp(spec)
    }
}

#[cfg(unix)]
fn bind_unix(path: &str) -> Result<SocketListener, String> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|error| format!("bind {path}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    Ok(SocketListener::Unix(listener))
}

#[cfg(windows)]
fn bind_unix(path: &str) -> Result<SocketListener, String> {
    Err(format!(
        "Unix-domain ListenStream={path} is not supported on Windows; use host:port"
    ))
}

fn bind_tcp(spec: &str) -> Result<SocketListener, String> {
    let address: std::net::SocketAddr = if let Ok(port) = spec.parse::<u16>() {
        std::net::SocketAddr::from(([0, 0, 0, 0], port))
    } else {
        spec.parse()
            .map_err(|error| format!("bad address `{spec}`: {error}"))?
    };
    let listener = TcpListener::bind(address).map_err(|error| format!("bind {spec}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    Ok(SocketListener::Tcp(listener))
}
