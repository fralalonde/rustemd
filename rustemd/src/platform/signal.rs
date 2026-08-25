//! Manager-level termination signals with Unix names on every platform.
//!
//! Unix converts these values to `nix::sys::signal::Signal`. Windows maps
//! graceful signals to a Job Object termination request because Win32 has no
//! general-purpose POSIX signal delivery API for arbitrary processes.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal(i32);

impl Signal {
    pub const SIGHUP: Self = Self(1);
    pub const SIGINT: Self = Self(2);
    pub const SIGQUIT: Self = Self(3);
    pub const SIGKILL: Self = Self(9);
    pub const SIGTERM: Self = Self(15);
    pub const SIGCHLD: Self = Self(17);

    pub const fn as_raw(self) -> i32 {
        self.0
    }

    #[cfg(unix)]
    pub(crate) fn to_nix(self) -> Option<nix::sys::signal::Signal> {
        nix::sys::signal::Signal::try_from(self.0).ok()
    }
}

impl TryFrom<i32> for Signal {
    type Error = ();
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        #[cfg(unix)]
        if nix::sys::signal::Signal::try_from(value).is_ok() {
            return Ok(Self(value));
        }
        #[cfg(windows)]
        if matches!(value, 1 | 2 | 3 | 9 | 15 | 17) {
            return Ok(Self(value));
        }
        Err(())
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.0 {
            1 => "SIGHUP",
            2 => "SIGINT",
            3 => "SIGQUIT",
            9 => "SIGKILL",
            15 => "SIGTERM",
            17 => "SIGCHLD",
            _ => return write!(f, "{}", self.0),
        };
        f.write_str(name)
    }
}
