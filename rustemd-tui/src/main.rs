//! `rustemd-tui` — terminal UI client for a running rustemd manager.

use std::io;
use std::process::ExitCode;

use clap::Parser;
use crossterm::{
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetSize, disable_raw_mode, enable_raw_mode,
    },
};

mod app;
mod render;
mod theme;

/// Draw size to fall back to when the terminal reports no usable window size.
/// A serial console has no `TIOCGWINSZ`, so crossterm reports 0×0.
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

#[derive(Parser)]
#[command(
    name = "rustemd-tui",
    version = rustemd::VERSION,
    about = "Terminal UI client for a running rustemd unit manager"
)]
struct Cli {
    /// Talk to the per-user manager instead of the system one.
    #[arg(long)]
    user: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.user) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rustemd-tui: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(user: bool) -> Result<(), String> {
    // Detect + connect to the running daemon before entering raw mode, so a
    // missing daemon prints a clean error on the ordinary terminal.
    let ctl = app::connect(user)?;

    let mut terminal = init_terminal()?;
    let result = app::App::new(ctl, user).run(&mut terminal);
    let restore = restore_terminal();
    result.map_err(|e| e.to_string())?;
    restore.map_err(|e| e.to_string())
}

fn init_terminal() -> Result<app::Terminal, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();

    // A serial console has no window size: `TIOCGWINSZ` reports 0×0, which
    // would give ratatui an empty draw area and no visible frame. Resolve a
    // usable size and, when the terminal reported none, pin ratatui to a
    // fixed 80×24 viewport so a frame is drawn anyway.
    let actual = crossterm::terminal::size().ok();
    let (cols, rows) = effective_size(actual);
    let viewport = match actual {
        Some((w, h)) if w > 0 && h > 0 => ratatui::Viewport::Fullscreen,
        _ => {
            // Ask a real terminal to resize itself (writes the xterm resize
            // sequence; a no-op on a dumb serial console — the fixed viewport
            // above is what actually guarantees a frame there).
            execute!(stdout, SetSize(cols, rows)).map_err(|e| e.to_string())?;
            ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, cols, rows))
        }
    };

    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    ratatui::Terminal::with_options(backend, ratatui::TerminalOptions { viewport })
        .map_err(|e| e.to_string())
}

/// Resolve a usable draw size: a missing or 0-sized terminal falls back to
/// [`DEFAULT_COLS`]×[`DEFAULT_ROWS`]; a real size is returned unchanged.
fn effective_size(actual: Option<(u16, u16)>) -> (u16, u16) {
    match actual {
        Some((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (DEFAULT_COLS, DEFAULT_ROWS),
    }
}

fn restore_terminal() -> Result<(), String> {
    disable_raw_mode().map_err(|e| e.to_string())?;
    execute!(io::stdout(), LeaveAlternateScreen).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_size_falls_back_when_terminal_is_unsized() {
        assert_eq!(effective_size(Some((0, 0))), (DEFAULT_COLS, DEFAULT_ROWS));
        assert_eq!(effective_size(Some((0, 24))), (DEFAULT_COLS, DEFAULT_ROWS));
        assert_eq!(effective_size(Some((100, 30))), (100, 30));
        assert_eq!(effective_size(None), (DEFAULT_COLS, DEFAULT_ROWS));
    }
}
