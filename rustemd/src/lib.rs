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
//! - `ipc` + `client` — JSON-line control over Unix sockets or Win32 named pipes.
//! - `daemon` — the PID-1 manager entry point (`rustemd daemon`).
//! - `unit` — unit-file parsing and typed unit configuration.
//! - `calendar` / `timespan` — systemd calendar expressions and time spans.
//! - `dbus` (Linux only, opt-in `dbus` feature) — `Type=dbus`/`BusName=`
//!   activation and a manager control interface, bridged to the poll loop
//!   over channels on a thread.
//!
//! The `systemctl`-compatible CLI lives in the sibling `rustemctl` crate,
//! which uses this library's `client`/`paths`/`enable`/`cli_style` modules.
//!
//! Platform note: Linux uses signalfd, cgroups/process groups, and optional
//! D-Bus. Windows uses named pipes, Job Objects, console controls, Winsock, and
//! the Service Control Manager. Boot, mounts, devices, and D-Bus remain Linux-only.

pub mod calendar;
pub mod cli_style;
pub mod client;
pub mod control;
pub mod daemon;
#[cfg(all(target_os = "linux", feature = "dbus"))]
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
