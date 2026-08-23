//! Color helpers for CLI stdout. Mirrors the palette used by the rest of
//! the tool: accent = cyan, active/current marker = green, warning = yellow,
//! default = magenta, error = red, dim = gray.
//!
//! All helpers return `String` (not `ColoredString`) so they compose with
//! plain strings in ternaries and `format!` without type errors.
//! The `colored` crate auto-disables ANSI when stdout is not a TTY or
//! `NO_COLOR` is set.

use colored::Colorize;

pub fn accent(s: &str) -> String {
    s.cyan().to_string()
}
pub fn star(s: &str) -> String {
    s.green().to_string()
}
pub fn warn(s: &str) -> String {
    s.yellow().to_string()
}
pub fn default_(s: &str) -> String {
    s.magenta().to_string()
}
pub fn error(s: &str) -> String {
    s.red().to_string()
}
pub fn dim(s: &str) -> String {
    s.truecolor(120, 120, 120).to_string()
}
pub fn ok(s: &str) -> String {
    s.green().to_string()
}
