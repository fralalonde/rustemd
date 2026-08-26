use std::fmt;
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    InvalidName(String),
    Parse { line: usize, message: String },
    AlreadyExists(String),
    NotFound(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::InvalidName(s) => write!(f, "invalid unit name: {s}"),
            Self::Parse { line, message } => write!(f, "line {line}: {message}"),
            Self::AlreadyExists(s) => write!(f, "unit definition already exists: {s}"),
            Self::NotFound(s) => write!(f, "unit definition not found: {s}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
