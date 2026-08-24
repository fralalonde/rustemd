//! The PID-1 manager daemon entry point. `rustemd` is the init/manager
//! binary; the `systemctl`-compatible control CLI lives in the separate
//! `rustemctl` crate. This module only exposes the `daemon` subcommand
//! (plus `--version`), which is what `/init` execs at boot.

use clap::{Parser, Subcommand};

use crate::cli_style as style;

#[derive(Parser)]
#[command(
    name = "rustemd",
    version = crate::VERSION,
    about = "A systemd init reimplementation: the PID-1 unit manager"
)]
pub struct Cli {
    /// Talk to the per-user manager instead of the system one.
    #[arg(long, global = true)]
    pub user: bool,

    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the manager daemon (init). `rustemd daemon --user`.
    #[command(name = "daemon", hide = true)]
    Daemon {
        /// Disable socket activation: load `.socket` units but bind/listen nothing.
        #[arg(long)]
        no_socket_activation: bool,
    },
    /// Show version.
    Version,
}

pub fn run(cli: Cli) -> i32 {
    match &cli.cmd {
        Command::Daemon {
            no_socket_activation,
        } => run_daemon(cli.user, *no_socket_activation),
        Command::Version => {
            println!("rustemd {}", crate::VERSION);
            0
        }
    }
}

pub fn run_daemon(user: bool, no_socket_activation: bool) -> i32 {
    // PID 1 boot: mount the API/virtual filesystems and run early-boot
    // configuration *before* reading the manager config, so the manager sees
    // the real hostname and a mounted /run (needed to bind its control
    // socket). Only compiled with the `boot` feature.
    #[cfg(feature = "boot")]
    if nix::unistd::getpid() == nix::unistd::Pid::from_raw(1) {
        if let Err(e) = crate::platform::boot::mount_api_filesystems() {
            eprintln!("rustemd: mount API filesystems failed: {e}");
        }
        crate::platform::boot::early_boot();
    }

    let mut cfg = match crate::manager::ManagerCfg::for_mode(user) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    cfg.socket_activation = !no_socket_activation;
    let mut mgr = match crate::manager::Manager::new(cfg) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let errs = mgr.load_all();
    for e in &errs {
        eprintln!("{}", style::warn(&format!("[load] {e}")));
    }

    // Discover kernel devices and begin monitoring uevents, so that
    // `After=sys-…device` / `Requires=sys-…device` ordering resolves before
    // the default target (and its dependencies) start.
    #[cfg(all(target_os = "linux", feature = "udev"))]
    mgr.udev_init();

    if !mgr.as_pid1 && !user {
        // Become a subreaper so orphaned daemonized children reparent to us.
        #[cfg(target_os = "linux")]
        nix::sys::prctl::set_child_subreaper(true).ok();
    }

    if let Err(e) = mgr.bind_ipc() {
        eprintln!("Failed to bind control socket: {e}");
        return 1;
    }
    if let Err(e) = mgr.bind_notify() {
        eprintln!("Failed to bind notify socket: {e}");
        return 1;
    }
    mgr.setup_signals();

    // Bring up the D-Bus bridge (Linux, opt-in `dbus` feature): Type=dbus/
    // BusName= activation plus a manager control interface. Best-effort — if
    // the bus is unreachable the bridge logs a warning and the manager keeps
    // running without D-Bus.
    #[cfg(all(target_os = "linux", feature = "dbus"))]
    if let Err(e) = mgr.start_dbus() {
        eprintln!("{}", style::warn(&format!("D-Bus: {e}")));
    }

    if mgr.as_pid1 || !user {
        // Boot into the default target.
        let _ = mgr.start("default.target");
    } else {
        let _ = mgr.start("default.target");
    }
    eprintln!("rustemd {} manager started", crate::VERSION);
    mgr.run();
    // Shutdown complete. As PID 1, power the machine off; elsewhere just exit.
    #[cfg(feature = "boot")]
    if nix::unistd::getpid() == nix::unistd::Pid::from_raw(1) {
        eprintln!("rustemd: shutdown complete, powering off");
        crate::platform::boot::poweroff();
    }
    0
}

/// Parse argv and run; returns the process exit code.
pub fn entry() -> i32 {
    match Cli::try_parse() {
        Ok(cli) => run(cli),
        Err(e) => {
            let _ = e.print();
            e.exit_code()
        }
    }
}
