//! rustemd — a systemd init reimplementation in Rust.
//!
//! A drop-in `systemctl` replacement with a built-in unit manager:
//! unit files, user services, timers, and dependency-driven lifecycle,
//! backed by per-service cgroups (Linux cgroup v2), with process groups and a
//! subreaper as the fallback where cgroups aren't available.
//!
//! Architecture:
//! - `manager` — the daemon: unit table, state machine, process supervision,
//!   timer engine, all behind a single-threaded poll loop.
//! - `ipc` + `client` — JSON-over-unix-socket control channel.
//! - `daemon` — the PID-1 manager entry point (`rustemd daemon`).
//! - `unit` — unit-file parsing and typed unit configuration.
//! - `calendar` / `timespan` — systemd calendar expressions and time spans.
//! - `dbus` (Linux only) — `Type=dbus`/`BusName=` activation and a manager
//!   control interface, bridged to the poll loop over channels on a thread.
//!
//! The `systemctl`-compatible CLI lives in the sibling `rustemctl` crate,
//! which uses this library's `client`/`paths`/`enable`/`cli_style` modules.
//!
//! Platform note: Linux is fully supported (signalfd, subreaper, process
//! groups, D-Bus). The non-Linux build paths exist but are not yet exercised;
//! the code keeps Linux-specific bits behind `cfg(target_os = "linux")`.

pub mod calendar;
pub mod cli_style;
pub mod client;
pub mod control;
pub mod daemon;
#[cfg(target_os = "linux")]
pub mod dbus;
pub mod enable;
pub mod ipc;
pub mod log;
pub mod manager;
pub mod names;
pub mod paths;
pub mod platform;
pub mod specifier;
pub mod timespan;
pub mod unit;

pub const VERSION: &str = env!("RUSTEMD_VERSION");
