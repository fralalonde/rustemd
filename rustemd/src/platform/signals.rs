//! Signal handling via `signalfd(2)`.
//!
//! The manager blocks the signals it handles and reads them through a
//! `signalfd`, which is pollable like any other fd — no async-signal-unsafe
//! work inside a handler.

use std::os::fd::{AsFd, BorrowedFd};

use nix::sys::signal::{self, SigSet, Signal};
use nix::sys::signalfd::{SfdFlags, SignalFd};

/// The signals the manager acts on.
pub const MANAGED_SIGNALS: [Signal; 5] = [
    Signal::SIGCHLD,
    Signal::SIGTERM,
    Signal::SIGINT,
    Signal::SIGQUIT,
    Signal::SIGHUP,
];

/// A pollable signal source. Drop it to stop listening.
pub struct SignalSource(SignalFd);

impl SignalSource {
    /// Block the manager's signals and install a `signalfd` for them. Also
    /// ignores `SIGPIPE` (writing to a dead child's pipe must not kill us).
    pub fn new() -> Option<SignalSource> {
        let mut set = SigSet::empty();
        for s in MANAGED_SIGNALS {
            set.add(s);
        }
        signal::sigprocmask(signal::SigmaskHow::SIG_BLOCK, Some(&set), None).ok()?;
        // SAFETY: installing SIG_IGN for SIGPIPE is a process-global but
        // idempotent disposition change done exactly once at startup.
        unsafe {
            signal::signal(Signal::SIGPIPE, signal::SigHandler::SigIgn).ok()?;
        }
        // Non-blocking so `read()` below drains pending signals without
        // stalling the event loop on a second read.
        let fd = SignalFd::with_flags(&set, SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC).ok()?;
        Some(SignalSource(fd))
    }

    /// The fd to register with `poll` for readability.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    /// Drain all currently-pending signals.
    pub fn read(&self) -> Vec<Signal> {
        let mut out = Vec::new();
        while let Ok(Some(info)) = self.0.read_signal() {
            if let Ok(s) = Signal::try_from(info.ssi_signo as i32) {
                out.push(s);
            }
        }
        out
    }
}
