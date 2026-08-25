//! The `systemctl`-compatible command surface. `rustemctl` is the control
//! CLI that talks to a running `rustemd` manager over its control socket;
//! the daemon itself lives in the `rustemd` crate. Invoking `rustemctl` as
//! `systemctl` (symlink) also enters CLI mode.

#![allow(clippy::too_many_arguments)]

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use colored::Colorize;
use serde_json::{Value, json};
use std::io::Write;

use rustemd::cli_style as style;
use rustemd::client::Client;
use rustemd::names::normalize_unit;
use rustemd::paths::Paths;

fn normalize_units(names: &[String]) -> Vec<String> {
    names.iter().map(|n| normalize_unit(n)).collect()
}

#[derive(Parser)]
#[command(
    name = "rustemctl",
    version = rustemd::VERSION,
    about = "systemctl-compatible control CLI for the rustemd unit manager"
)]
pub struct Cli {
    /// Talk to the per-user manager instead of the system one.
    #[arg(long, global = true)]
    pub user: bool,
    /// Do not pipe output through a pager.
    #[arg(long, global = true)]
    pub no_pager: bool,

    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start (activate) one or more units.
    Start { units: Vec<String> },
    /// Stop (deactivate) one or more units.
    Stop { units: Vec<String> },
    /// Restart one or more units.
    Restart { units: Vec<String> },
    /// Run a unit's ExecReload.
    Reload { units: Vec<String> },
    /// Stop and then start units; if not active, just start.
    RestartOrStart { units: Vec<String> },
    /// Send a signal to a unit's main process group.
    Kill {
        unit: String,
        #[arg(long)]
        signal: Option<String>,
    },
    /// Show runtime status of one or more units (or all).
    Status {
        units: Vec<String>,
        /// Do not truncate output.
        #[arg(short = 'l', long)]
        full: bool,
    },
    /// Query whether units are active.
    #[command(name = "is-active")]
    IsActive { units: Vec<String> },
    /// Query whether units are in a failed state.
    #[command(name = "is-failed")]
    IsFailed { units: Vec<String> },
    /// Query whether units are enabled.
    #[command(name = "is-enabled")]
    IsEnabled { units: Vec<String> },
    /// Enable units (create [Install] symlinks).
    Enable {
        units: Vec<String>,
        /// Also start the units right away.
        #[arg(long)]
        now: bool,
    },
    /// Disable units (remove [Install] symlinks).
    Disable {
        units: Vec<String>,
        /// Also stop the units right away.
        #[arg(long)]
        now: bool,
    },
    /// Reload the manager's unit configuration from disk.
    #[command(name = "daemon-reload")]
    DaemonReload,
    /// List units and their states.
    #[command(name = "list-units")]
    ListUnits {
        #[arg(long, value_delimiter = ',')]
        type_: Option<Vec<String>>,
        #[arg(long)]
        state: Option<String>,
        /// Do not print the header line.
        #[arg(long)]
        no_legend: bool,
        #[arg(long)]
        plain: bool,
    },
    /// List installed unit files and their enablement state.
    #[command(name = "list-unit-files")]
    ListUnitFiles {
        #[arg(long)]
        no_legend: bool,
    },
    /// List timers and their next/last elapse.
    #[command(name = "list-timers")]
    ListTimers {
        #[arg(long)]
        no_legend: bool,
        #[arg(long)]
        all: bool,
    },
    /// Show the contents of unit files.
    Cat { units: Vec<String> },
    /// Show unit properties.
    Show {
        units: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        property: Vec<String>,
        #[arg(long)]
        value: bool,
    },
    /// Print the current default target.
    #[command(name = "get-default")]
    GetDefault,
    /// Set the default target.
    #[command(name = "set-default")]
    SetDefault { target: String },
    /// Start a target and stop units not required by it.
    Isolate { target: String },
    /// Whether the manager is running without failed units.
    #[command(name = "is-system-running")]
    IsSystemRunning,
    /// Orderly stop all units and exit.
    #[command(name = "poweroff")]
    Poweroff,
    /// Show version.
    Version,
    /// Generate shell completions.
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(ValueEnum, Clone, Copy)]
pub enum Shell {
    Bash,
    Fish,
    Zsh,
    #[value(name = "powershell")]
    PowerShell,
    Nushell,
}

pub fn run(cli: Cli) -> i32 {
    let res = match &cli.cmd {
        Command::Version => {
            println!("rustemctl {}", rustemd::VERSION);
            return 0;
        }
        command => dispatch(&cli, command),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {}", style::error("Failed:"), e);
            1
        }
    }
}

fn paths(user: bool) -> Result<Paths, String> {
    if user {
        Paths::user()
    } else {
        Ok(Paths::system())
    }
}

fn dispatch(cli: &Cli, cmd: &Command) -> Result<i32, String> {
    match cmd {
        Command::Version => Ok(0),
        Command::Start { units } => units_op(cli, "start", units).map(|_| 0),
        Command::Stop { units } => units_op(cli, "stop", units).map(|_| 0),
        Command::Restart { units } => units_op(cli, "restart", units).map(|_| 0),
        Command::Reload { units } => units_op(cli, "reload", units).map(|_| 0),
        Command::RestartOrStart { units } => units_op(cli, "restart", units).map(|_| 0),
        Command::Kill { unit, signal } => {
            let client = Client::for_mode(cli.user)?;
            let sig = signal
                .as_deref()
                .and_then(rustemd::unit::sig_from_name)
                .unwrap_or(rustemd::platform::signal::Signal::SIGTERM);
            client
                .op_with(
                    "kill",
                    json!({"units": [normalize_unit(unit)], "signal": format!("{sig}")}),
                )
                .map(|_| 0)
        }
        Command::Status { units, full } => cmd_status(cli, units, *full),
        Command::IsActive { units } => cmd_is_active(cli, units),
        Command::IsFailed { units } => cmd_is_failed(cli, units),
        Command::IsEnabled { units } => cmd_is_enabled(cli, units),
        Command::Enable { units, now } => cmd_enable(cli, units, *now),
        Command::Disable { units, now } => cmd_disable(cli, units, *now),
        Command::DaemonReload => {
            let client = Client::for_mode(cli.user)?;
            client.simple_op("daemon_reload").map(|_| 0)
        }
        Command::ListUnits {
            type_,
            state,
            no_legend,
            plain,
        } => cmd_list_units(
            cli,
            type_.clone().unwrap_or_default(),
            state.clone(),
            *no_legend,
            *plain,
        ),
        Command::ListUnitFiles { no_legend } => cmd_list_unit_files(cli, *no_legend),
        Command::ListTimers { no_legend, all } => cmd_list_timers(cli, *no_legend, *all),
        Command::Cat { units } => cmd_cat(cli, units),
        Command::Show {
            units,
            property,
            value,
        } => cmd_show(cli, units, property, *value),
        Command::GetDefault => {
            let client = Client::for_mode(cli.user)?;
            let v = client.simple_op("get_default")?;
            println!("{}", v.as_str().unwrap_or("default.target"));
            Ok(0)
        }
        Command::SetDefault { target } => {
            let client = Client::for_mode(cli.user)?;
            client
                .op_with("set_default", json!({"name": normalize_unit(target)}))
                .map(|_| 0)
        }
        Command::Isolate { target } => {
            let client = Client::for_mode(cli.user)?;
            client
                .op_with("isolate", json!({"name": normalize_unit(target)}))
                .map(|_| 0)
        }
        Command::IsSystemRunning => {
            let client = Client::for_mode(cli.user)?;
            let v = client.simple_op("is_system_running")?;
            if v.get("degraded").and_then(Value::as_bool).unwrap_or(false) {
                println!("degraded");
                Ok(1)
            } else {
                println!("running");
                Ok(0)
            }
        }
        Command::Poweroff => {
            let client = Client::for_mode(cli.user)?;
            client.simple_op("shutdown").map(|_| 0)
        }
        Command::Completions { shell } => {
            cmd_completions(*shell);
            Ok(0)
        }
    }
}

/// Generate a shell completion script for the invoked binary. Derives the
/// program name from `argv[0]` so `systemctl completions bash` emits
/// completions for `systemctl`, not the internal `rustemctl` name.
fn cmd_completions(shell: Shell) {
    let mut cmd = Cli::command();
    let name = std::env::args()
        .next()
        .and_then(|a| {
            std::path::Path::new(&a)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| cmd.get_name().to_string());
    // Buffer first, then write: `clap_complete` panics on write errors, so
    // `systemctl completions fish | head` would abort with a broken-pipe
    // panic instead of exiting quietly.
    let mut buf = Vec::new();
    match shell {
        Shell::Bash => {
            clap_complete::generate(clap_complete::shells::Bash, &mut cmd, &name, &mut buf)
        }
        Shell::Fish => {
            clap_complete::generate(clap_complete::shells::Fish, &mut cmd, &name, &mut buf)
        }
        Shell::Zsh => {
            clap_complete::generate(clap_complete::shells::Zsh, &mut cmd, &name, &mut buf)
        }
        Shell::PowerShell => {
            clap_complete::generate(clap_complete::shells::PowerShell, &mut cmd, &name, &mut buf)
        }
        Shell::Nushell => {
            clap_complete::generate(clap_complete_nushell::Nushell, &mut cmd, &name, &mut buf)
        }
    }
    let _ = std::io::stdout().write_all(&buf);
}

fn client_for(cli: &Cli) -> Result<Client, String> {
    Client::for_mode(cli.user)
}

fn units_op(cli: &Cli, op: &str, units: &[String]) -> Result<(), String> {
    let client = client_for(cli)?;
    let norm = normalize_units(units);
    client.units_op(op, &norm)?;
    Ok(())
}

// ---- command handlers ---------------------------------------------------------

fn cmd_is_active(cli: &Cli, units: &[String]) -> Result<i32, String> {
    let client = client_for(cli)?;
    let norm = normalize_units(units);
    let v = client.units_op("is_active", &norm)?;
    let states: Vec<(String, String)> = v["states"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    (
                        x[0].as_str().unwrap_or("").to_string(),
                        x[1].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    for (_, s) in &states {
        println!("{s}");
    }
    Ok(v.get("exit").and_then(Value::as_i64).unwrap_or(0) as i32)
}

fn cmd_is_failed(cli: &Cli, units: &[String]) -> Result<i32, String> {
    let client = client_for(cli)?;
    let norm = normalize_units(units);
    let v = client.units_op("is_failed", &norm)?;
    let failed = v
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if failed {
        println!("failed");
        Ok(0)
    } else {
        println!("active");
        Ok(1)
    }
}

fn cmd_is_enabled(cli: &Cli, units: &[String]) -> Result<i32, String> {
    let paths = paths(cli.user)?;
    let mut worst = 0;
    for u in units {
        let state = rustemd::enable::enabled_state(&paths, u);
        println!("{state}");
        worst = worst.max(match state.as_str() {
            "not-found" => 3,
            "disabled" | "masked" => 1,
            _ => 0,
        });
    }
    Ok(worst)
}

fn cmd_enable(cli: &Cli, units: &[String], now: bool) -> Result<i32, String> {
    let paths = paths(cli.user)?;
    for u in units {
        let norm = normalize_unit(u);
        let msgs = rustemd::enable::enable(&paths, &norm)?;
        for m in msgs {
            println!("{}", style::ok(&m));
        }
    }
    if now {
        let client = client_for(cli)?;
        let norm = normalize_units(units);
        client.units_op("start", &norm)?;
    } else {
        // Best-effort daemon reload so the change takes effect.
        if let Ok(client) = client_for(cli) {
            let _ = client.simple_op("daemon_reload");
        }
    }
    Ok(0)
}

fn cmd_disable(cli: &Cli, units: &[String], now: bool) -> Result<i32, String> {
    let paths = paths(cli.user)?;
    for u in units {
        let norm = normalize_unit(u);
        let msgs = rustemd::enable::disable(&paths, &norm)?;
        for m in msgs {
            println!("{}", style::ok(&m));
        }
    }
    if now {
        let client = client_for(cli)?;
        let norm = normalize_units(units);
        client.units_op("stop", &norm)?;
    }
    Ok(0)
}

fn cmd_status(cli: &Cli, units: &[String], _full: bool) -> Result<i32, String> {
    let client = client_for(cli)?;
    let norm = normalize_units(units);
    let v = client.units_op("status", &norm)?;
    let arr = v.as_array().ok_or("bad status response")?;
    for (i, u) in arr.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_unit_status(u);
    }
    Ok(0)
}

fn print_unit_status(u: &Value) {
    let name = u.get("name").and_then(Value::as_str).unwrap_or("?");
    let desc = u.get("description").and_then(Value::as_str).unwrap_or("");
    let active = u.get("active").and_then(Value::as_str).unwrap_or("?");
    let load = u.get("load").and_then(Value::as_str).unwrap_or("?");
    let sub = u.get("sub").and_then(Value::as_str).unwrap_or("?");
    let active_color = match active {
        "active" => style::star("●"),
        "failed" => style::error("●"),
        _ => "●".to_string(),
    };
    println!("{} {} - {}", active_color, style::accent(name), desc);
    let path = u.get("path").and_then(Value::as_str).unwrap_or("");
    let enabled = u.get("enabled").and_then(Value::as_str).unwrap_or("");
    println!(
        "     Loaded: {} ({}{})",
        style::warn(load),
        path,
        if enabled.is_empty() {
            String::new()
        } else {
            format!("; {enabled}")
        }
    );
    let since = u
        .get("active_enter")
        .and_then(Value::as_u64)
        .map(fmt_epoch)
        .unwrap_or_else(|| "-".into());
    let main_pid = u.get("main_pid").and_then(Value::as_i64).unwrap_or(0);
    println!(
        "     Active: {} ({}) since {}",
        style::warn(active),
        style::dim(sub),
        since
    );
    if main_pid > 0 {
        println!("   Main PID: {main_pid}");
    }
    let log = u
        .get("log")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !log.is_empty() {
        println!("      Logs:");
        for line in log.iter().take(20) {
            println!("        {}", style::dim(line.as_str().unwrap_or("")));
        }
    }
}

fn fmt_epoch(secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dt = chrono::DateTime::from_timestamp(secs as i64, 0);
    let s = dt
        .map(|d| d.format("%a %Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    let ago = now.saturating_sub(secs);
    // `fmt_ago` already includes the "ago" suffix.
    format!(
        "{}; {}",
        s,
        rustemd::timespan::fmt_ago(std::time::Duration::from_secs(ago))
    )
}

fn cmd_list_units(
    cli: &Cli,
    types: Vec<String>,
    state: Option<String>,
    no_legend: bool,
    _plain: bool,
) -> Result<i32, String> {
    let client = client_for(cli)?;
    let v = client.op_with(
        "list_units",
        json!({"types": types, "state": state, "pattern": null}),
    )?;
    let rows = v.as_array().unwrap_or(&vec![]).clone();
    if !no_legend {
        println!(
            "{:<32} {:<8} {:<8} {:<10} DESCRIPTION",
            "UNIT", "LOAD", "ACTIVE", "SUB"
        );
    }
    for r in &rows {
        println!(
            "{:<32} {:<8} {:<8} {:<10} {}",
            r["unit"].as_str().unwrap_or(""),
            r["loaded"].as_str().unwrap_or(""),
            r["active"].as_str().unwrap_or(""),
            r["sub"].as_str().unwrap_or(""),
            r["description"].as_str().unwrap_or(""),
        );
    }
    Ok(0)
}

fn cmd_list_unit_files(cli: &Cli, no_legend: bool) -> Result<i32, String> {
    let client = client_for(cli)?;
    let v = client.simple_op("list_unit_files")?;
    let rows = v.as_array().unwrap_or(&vec![]).clone();
    if !no_legend {
        println!("{:<32} STATE", "UNIT FILE");
    }
    for r in &rows {
        println!(
            "{:<32} {}",
            r["file"].as_str().unwrap_or(""),
            style_state(r["state"].as_str().unwrap_or("")),
        );
    }
    Ok(0)
}

fn style_state(s: &str) -> String {
    match s {
        "enabled" => s.green().to_string(),
        "disabled" => s.yellow().to_string(),
        _ => s.to_string(),
    }
}

fn cmd_list_timers(cli: &Cli, no_legend: bool, _all: bool) -> Result<i32, String> {
    let client = client_for(cli)?;
    let v = client.simple_op("list_timers")?;
    let rows = v.as_array().unwrap_or(&vec![]).clone();
    if !no_legend {
        println!(
            "{:<26} {:<12} {:<26} {:<12} {:<20} ACTIVATES",
            "NEXT", "LEFT", "LAST", "PASSED", "UNIT"
        );
    }
    for r in &rows {
        let next = r
            .get("next")
            .and_then(Value::as_u64)
            .map(fmt_epoch)
            .unwrap_or_else(|| "-".into());
        let next_left = r
            .get("next_left")
            .and_then(Value::as_i64)
            .filter(|d| *d > 0)
            .map(|d| {
                format!(
                    "{} left",
                    rustemd::timespan::fmt_left(std::time::Duration::from_secs(d.max(0) as u64))
                )
            })
            .unwrap_or_else(String::new);
        let last = r
            .get("last")
            .and_then(Value::as_u64)
            .map(fmt_epoch)
            .unwrap_or_else(|| "-".into());
        let last_passed = r
            .get("last_passed")
            .and_then(Value::as_i64)
            .filter(|d| *d >= 0)
            .map(|d| rustemd::timespan::fmt_ago(std::time::Duration::from_secs(d as u64)))
            .unwrap_or_else(String::new);
        println!(
            "{:<26} {:<12} {:<26} {:<12} {:<20} {}",
            style::accent(&next),
            next_left,
            last,
            last_passed,
            r["unit"].as_str().unwrap_or(""),
            r["activates"].as_str().unwrap_or(""),
        );
    }
    Ok(0)
}

fn cmd_cat(cli: &Cli, units: &[String]) -> Result<i32, String> {
    let client = client_for(cli)?;
    let norm = normalize_units(units);
    let v = client.units_op("cat", &norm)?;
    let rows = v.as_array().unwrap_or(&vec![]).clone();
    for r in &rows {
        println!(
            "# {}\n# {}",
            r["unit"].as_str().unwrap_or(""),
            r["path"].as_str().unwrap_or("")
        );
        println!("{}", r["text"].as_str().unwrap_or(""));
    }
    Ok(0)
}

fn cmd_show(
    cli: &Cli,
    units: &[String],
    property: &[String],
    value_only: bool,
) -> Result<i32, String> {
    let client = client_for(cli)?;
    let norm = normalize_units(units);
    let v = client.units_op("show", &norm)?;
    let rows = v.as_array().unwrap_or(&vec![]).clone();
    for r in &rows {
        let obj = r.as_object().unwrap();
        for (k, val) in obj {
            if !property.is_empty() && !property.contains(k) {
                continue;
            }
            let vs = val
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| val.to_string());
            if value_only {
                println!("{vs}");
            } else {
                println!("{k}={vs}");
            }
        }
        println!();
    }
    Ok(0)
}

/// Parse argv and run; returns the process exit code. Used by both the
/// `rustemctl` and `systemctl` binaries (symlink drop-in).
pub fn entry() -> i32 {
    match Cli::try_parse() {
        Ok(cli) => run(cli),
        Err(e) => {
            let _ = e.print();
            e.exit_code()
        }
    }
}
