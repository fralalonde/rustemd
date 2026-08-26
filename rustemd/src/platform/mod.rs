//! Platform layer — the OS-specific surface of rustemd.
//!
//! Raw OS operations live here behind small interfaces consumed by the manager
//! state machine ([`crate::manager`]). Unix implementations use `nix`; Windows
//! implementations use direct Win32 bindings for named pipes, Job Objects,
//! console controls, Winsock readiness, filesystem links, and SCM hosting.
//! Unsupported platform behavior fails explicitly rather than becoming a no-op.

#[cfg(all(unix, feature = "boot"))]
pub mod boot;
#[cfg(unix)]
pub mod cgroup;
pub mod fs;
#[cfg(target_os = "linux")]
pub mod mount;
#[cfg(unix)]
pub mod net;
#[cfg(windows)]
#[path = "windows/net.rs"]
pub mod net;
#[cfg(unix)]
pub mod process;
#[cfg(windows)]
#[path = "windows/process.rs"]
pub mod process;
#[cfg(target_os = "linux")]
pub mod sandbox;
#[cfg(windows)]
pub mod service;
pub mod signal;
#[cfg(unix)]
pub mod signals;
#[cfg(windows)]
#[path = "windows/signals.rs"]
pub mod signals;
#[cfg(all(target_os = "linux", feature = "udev"))]
pub mod udev;
