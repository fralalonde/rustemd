//! Color palette and shared styles for the rustemd TUI.

use ratatui::style::{Color, Style};

pub(crate) const C_BORDER: Color = Color::DarkGray;
pub(crate) const C_BORDER_ACTIVE: Color = Color::Cyan;
pub(crate) const C_ACCENT: Color = Color::Cyan;
pub(crate) const C_ACTIVE: Color = Color::Green;
pub(crate) const C_WARN: Color = Color::Yellow;
pub(crate) const C_ERROR: Color = Color::Red;
pub(crate) const C_DIM: Color = Color::Gray;
pub(crate) const C_INFO: Color = Color::LightCyan;
pub(crate) const C_HIGHLIGHT_BG: Color = Color::Blue;

pub(crate) fn highlight_style() -> Style {
    Style::default().bg(C_HIGHLIGHT_BG).fg(Color::White)
}

/// Color for a systemd `active` state string.
pub(crate) fn state_color(state: &str) -> Color {
    match state {
        "active" | "running" => C_ACTIVE,
        "failed" => C_ERROR,
        "activating" | "deactivating" | "reloading" => C_WARN,
        _ => C_DIM,
    }
}

/// Color for a `[Install]` enablement state string.
pub(crate) fn enabled_color(state: &str) -> Color {
    match state {
        "enabled" | "enabled-runtime" | "static" => C_ACTIVE,
        "masked" => C_WARN,
        _ => C_DIM,
    }
}
