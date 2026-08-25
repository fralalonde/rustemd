//! The repository crate's error type.

use std::fmt;

/// An error from the repository layer.
#[derive(Debug)]
pub enum Error {
    /// An underlying I/O failure.
    Io(std::io::Error),
    /// A `git` command failed (or `git` was unavailable when required).
    Git(String),
    /// A unit file name was rejected (path traversal, no recognized suffix).
    InvalidName(String),
    /// `create` refused to overwrite an existing unit file.
    AlreadyExists(String),
    /// `update` refused to create a unit file that does not exist.
    NotFound(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Git(s) => write!(f, "git: {s}"),
            Error::InvalidName(s) => write!(f, "invalid unit name: {s}"),
            Error::AlreadyExists(s) => write!(f, "unit file already exists: {s}"),
            Error::NotFound(s) => write!(f, "unit file not found: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
