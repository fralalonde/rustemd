//! Platform layer — the OS-specific surface of rustemd.
//!
//! Every raw Linux/unix syscall (process spawning, group kill, reaping,
//! `signalfd`, unix-socket IPC) lives here, behind small, documented
//! functions. The manager's state machine ([`crate::manager`]) talks only to
//! this module for process/signal/socket work, so porting to a new OS means
//! reimplementing these three submodules, not auditing the whole codebase.
//!
//! The non-unix build is a stub that fails loudly at runtime; nothing here is
//! silently "no-op" where a real behaviour is expected.

#[cfg(all(unix, feature = "boot"))]
pub mod boot;
#[cfg(unix)]
pub mod cgroup;
#[cfg(unix)]
pub mod net;
#[cfg(unix)]
pub mod process;
#[cfg(unix)]
pub mod signals;
