//! The manager daemon and Windows SCM entry points. `rustemd` is the init/manager
//! binary; the `systemctl`-compatible control CLI lives in the separate
//! `rustemctl` crate. This module only exposes the `daemon` subcommand
//! (plus `--version`), which is what `/init` execs at boot.

use clap::{Parser, Subcommand};

use crate::cli_style as style;

#[derive(Parser)]
#[command(
    name = "rustemd",
    version = crate::VERSION,
    about = "A systemd-compatible unit manager"
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
    /// Install, remove, or run the native Windows Service Control Manager host.
    #[cfg(windows)]
    Service {
        #[command(subcommand)]
        action: ServiceCommand,
    },
    /// Show version.
    Version,
}

#[cfg(windows)]
#[derive(Subcommand)]
pub enum ServiceCommand {
    /// Enter the Service Control Dispatcher. Invoked by SCM, not interactively.
    Run {
        #[arg(long, default_value = "rustemd")]
        name: String,
    },
    /// Register rustemd as a Windows service. Requires an elevated terminal.
    Install {
        #[arg(long, default_value = "rustemd")]
        name: String,
        #[arg(long, default_value = "rustemd unit manager")]
        display_name: String,
        /// Use demand start instead of automatic start.
        #[arg(long)]
        manual: bool,
    },
    /// Remove the Windows service registration. Requires an elevated terminal.
    Uninstall {
        #[arg(long, default_value = "rustemd")]
        name: String,
    },
}

pub fn run(cli: Cli) -> i32 {
    match &cli.cmd {
        Command::Daemon {
            no_socket_activation,
        } => run_daemon(cli.user, *no_socket_activation),
        #[cfg(windows)]
        Command::Service { action } => match action {
            ServiceCommand::Run { name } => {
                service_result(crate::platform::service::run_dispatcher(name))
            }
            ServiceCommand::Install {
                name,
                display_name,
                manual,
            } => service_result(crate::platform::service::install(
                name,
                display_name,
                *manual,
            )),
            ServiceCommand::Uninstall { name } => {
                service_result(crate::platform::service::uninstall(name))
            }
        },
        Command::Version => {
            println!("rustemd {}", crate::VERSION);
            0
        }
    }
}

#[cfg(windows)]
fn service_result(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("rustemd: {error}");
            1
        }
    }
}

pub fn run_daemon(user: bool, no_socket_activation: bool) -> i32 {
    run_daemon_with_ready(user, no_socket_activation, || {})
}

pub(crate) fn run_daemon_with_ready(
    user: bool,
    no_socket_activation: bool,
    ready: impl FnOnce(),
) -> i32 {
    // PID 1 boot: mount the API/virtual filesystems and run early-boot
    // configuration *before* reading the manager config, so the manager sees
    // the real hostname and a mounted /run (needed to bind its control
    // socket). Only compiled with the `boot` feature.
    #[cfg(all(unix, feature = "boot"))]
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
    ready();
    eprintln!("rustemd {} manager started", crate::VERSION);
    mgr.run();
    // Shutdown complete. As PID 1, power the machine off; elsewhere just exit.
    #[cfg(all(unix, feature = "boot"))]
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

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn parses_windows_service_install_and_uninstall_commands() {
        let install = Cli::try_parse_from([
            "rustemd",
            "service",
            "install",
            "--name",
            "rustemd-test",
            "--manual",
        ])
        .unwrap();
        assert!(matches!(
            install.cmd,
            Command::Service { action: ServiceCommand::Install { ref name, manual: true, .. } }
                if name == "rustemd-test"
        ));

        let uninstall =
            Cli::try_parse_from(["rustemd", "service", "uninstall", "--name", "rustemd-test"])
                .unwrap();
        assert!(matches!(
            uninstall.cmd,
            Command::Service { action: ServiceCommand::Uninstall { ref name } }
                if name == "rustemd-test"
        ));
    }

    #[test]
    fn service_image_path_quotes_executable() {
        let path = std::path::Path::new(r"C:\\Program Files\\rustemd\\rustemd.exe");
        assert_eq!(
            crate::platform::service::service_image_path(path),
            r#""C:\\Program Files\\rustemd\\rustemd.exe" service run"#
        );
    }
}
