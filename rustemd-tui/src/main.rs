//! `rustemd-tui` — terminal UI client for a running rustemd manager.

use std::io;
use std::process::ExitCode;

use clap::Parser;
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

mod app;
mod render;
mod theme;

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
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    ratatui::Terminal::new(backend).map_err(|e| e.to_string())
}

fn restore_terminal() -> Result<(), String> {
    disable_raw_mode().map_err(|e| e.to_string())?;
    execute!(io::stdout(), LeaveAlternateScreen).map_err(|e| e.to_string())?;
    Ok(())
}
