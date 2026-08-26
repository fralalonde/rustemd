//! Socket-activation listener types and binding. Compiled only with the
//! `socket` feature; the manager core calls these from its `SocketUnit` path.

use std::net::TcpListener;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::{UnixDatagram, UnixListener};
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

#[cfg(unix)]
pub type SocketId = RawFd;
#[cfg(windows)]
pub type SocketId = usize;

/// A bound, non-blocking listening socket for a `.socket` unit.
#[derive(Debug)]
pub enum SocketListener {
    #[cfg(unix)]
    Unix(UnixListener),
    #[cfg(unix)]
    Datagram(UnixDatagram),
    /// `SOCK_SEQPACKET` unix socket. std has no seqpkt type, so the owned fd
    /// is stored directly.
    #[cfg(unix)]
    SeqPacket(OwnedFd),
    /// Netlink socket; owned-fd-backed because std has no netlink type.
    #[cfg(unix)]
    Netlink(OwnedFd),
    Tcp(TcpListener),
}

impl SocketListener {
    pub fn id(&self) -> SocketId {
        match self {
            #[cfg(unix)]
            SocketListener::Unix(listener) => listener.as_raw_fd(),
            #[cfg(unix)]
            SocketListener::Datagram(sock) => sock.as_raw_fd(),
            #[cfg(unix)]
            SocketListener::SeqPacket(fd) | SocketListener::Netlink(fd) => fd.as_raw_fd(),
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

/// Bind a `ListenDatagram=` spec. Only Unix-domain datagram sockets are
/// supported (UDP/IP multicast is not); non-`unix:`/absolute specs are
/// rejected like abstract sockets.
#[cfg(unix)]
pub fn bind_listen_datagram(spec: &str) -> Result<SocketListener, String> {
    if let Some(path) = spec.strip_prefix("unix:") {
        bind_unix_datagram(path)
    } else if spec.starts_with('/') {
        bind_unix_datagram(spec)
    } else if spec.starts_with('@') {
        Err(format!("abstract unix sockets not supported: {spec}"))
    } else {
        Err(format!("only unix: datagram sockets are supported: {spec}"))
    }
}

#[cfg(windows)]
pub fn bind_listen_datagram(spec: &str) -> Result<SocketListener, String> {
    Err(format!(
        "ListenDatagram={spec} requires Unix-domain sockets, not supported on Windows"
    ))
}

#[cfg(unix)]
fn bind_unix_datagram(path: &str) -> Result<SocketListener, String> {
    let _ = std::fs::remove_file(path);
    let sock = UnixDatagram::bind(path).map_err(|error| format!("bind {path}: {error}"))?;
    sock.set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    Ok(SocketListener::Datagram(sock))
}

/// Bind a `ListenSequentialPacket=` spec: a Unix `SOCK_SEQPACKET` socket
/// created via `nix` (std has no seqpkt type). The raw fd is the stored
/// handle so it can be polled like the other listeners.
#[cfg(unix)]
pub fn bind_listen_sequential_packet(spec: &str) -> Result<SocketListener, String> {
    if let Some(path) = spec.strip_prefix("unix:") {
        bind_unix_seqpacket(path)
    } else if spec.starts_with('/') {
        bind_unix_seqpacket(spec)
    } else if spec.starts_with('@') {
        Err(format!("abstract unix sockets not supported: {spec}"))
    } else {
        Err(format!(
            "only unix: sequential-packet sockets are supported: {spec}"
        ))
    }
}

#[cfg(unix)]
fn bind_unix_seqpacket(path: &str) -> Result<SocketListener, String> {
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, bind, socket};

    let _ = std::fs::remove_file(path);
    let fd = socket(
        AddressFamily::Unix,
        SockType::SeqPacket,
        SockFlag::empty(),
        None,
    )
    .map_err(|error| format!("socket seqpkt {path}: {error}"))?;
    let addr = UnixAddr::new(path).map_err(|error| format!("addr {path}: {error}"))?;
    bind(fd.as_raw_fd(), &addr).map_err(|error| format!("bind {path}: {error}"))?;
    nix::fcntl::fcntl(
        &fd,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .map_err(|error| format!("set nonblock {path}: {error}"))?;
    // Store the owned fd so it closes when the manager drops the listener on
    // socket-unit stop.
    Ok(SocketListener::SeqPacket(fd))
}

#[cfg(windows)]
pub fn bind_listen_sequential_packet(spec: &str) -> Result<SocketListener, String> {
    Err(format!(
        "ListenSequentialPacket={spec} requires Unix-domain sockets, not supported on Windows"
    ))
}

/// Bind a `ListenNetlink=` spec. The spec is a netlink *family name* (e.g.
/// `kobject-uevent`, `route`); the socket is bound to the kernel. `nix` 0.31
/// has no netlink address family, so this is done via `libc` directly.
#[cfg(unix)]
pub fn bind_listen_netlink(spec: &str) -> Result<SocketListener, String> {
    let family = netlink_family(spec).ok_or_else(|| format!("unknown netlink family `{spec}`"))?;
    // socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC | SOCK_NONBLOCK, family)
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            family as libc::c_int,
        )
    };
    if fd < 0 {
        return Err(format!(
            "socket netlink {spec}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // Zero-init avoids touching libc's (private) padding fields; set the
    // public ones explicitly.
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // nl_pid == 0: bound to the kernel.
    addr.nl_pid = 0;
    addr.nl_groups = 0;
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("bind netlink {spec}: {err}"));
    }
    Ok(SocketListener::Netlink(unsafe { OwnedFd::from_raw_fd(fd) }))
}

#[cfg(windows)]
pub fn bind_listen_netlink(spec: &str) -> Result<SocketListener, String> {
    Err(format!(
        "ListenNetlink={spec} requires netlink sockets, not supported on Windows"
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

/// Map a netlink family name to its `NETLINK_*` number (defined in
/// `<linux/netlink.h>`). Order/numbers follow the kernel UAPI.
#[cfg(unix)]
fn netlink_family(name: &str) -> Option<u32> {
    Some(match name {
        "route" => 0,           // NETLINK_ROUTE
        "sock-diag" => 4,       // NETLINK_SOCK_DIAG
        "nflog" => 5,           // NETLINK_NFLOG
        "xfrm" => 6,            // NETLINK_XFRM
        "selinux" => 7,         // NETLINK_SELINUX
        "audit" => 9,           // NETLINK_AUDIT
        "nfnetlink" => 12,      // NETLINK_NFNETLINK
        "kobject-uevent" => 15, // NETLINK_KOBJECT_UEVENT
        _ => return None,
    })
}

#[cfg(all(test, feature = "socket", unix))]
mod tests {
    use super::*;

    #[test]
    fn datagram_listener_binds() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("rustemd-dgram-{}.sock", std::process::id()));
        let path = path.to_str().unwrap();
        let _ = std::fs::remove_file(path);
        let listener = bind_listen_datagram(path).unwrap();
        let fd = listener.id();
        assert!(fd > 0, "expected a valid deadline fd, got {fd}");
        // The bound path exists until the listener is dropped.
        assert!(std::path::Path::new(path).exists());
        std::mem::drop(listener);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn datagram_rejects_udp_spec() {
        let err = bind_listen_datagram("127.0.0.1:8123").unwrap_err();
        assert!(err.contains("only unix"), "unexpected error: {err}");
    }

    #[test]
    fn seqpacket_listener_binds() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("rustemd-seqpkt-{}.sock", std::process::id()));
        let path = path.to_str().unwrap();
        let _ = std::fs::remove_file(path);
        let listener = bind_listen_sequential_packet(path).unwrap();
        let fd = listener.id();
        assert!(fd > 0, "expected a valid fd, got {fd}");
        assert!(std::path::Path::new(path).exists());
        std::mem::drop(listener);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn netlink_kobject_uevent_binds() {
        let listener = bind_listen_netlink("kobject-uevent").unwrap();
        let fd = listener.id();
        assert!(fd > 0, "expected a valid fd, got {fd}");
        std::mem::drop(listener);
    }

    #[test]
    fn netlink_rejects_unknown_family() {
        let err = bind_listen_netlink("does-not-exist").unwrap_err();
        assert!(
            err.contains("unknown netlink family"),
            "unexpected error: {err}"
        );
    }
}
