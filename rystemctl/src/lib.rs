//! `rystemctl` — the `systemctl`-compatible control CLI for rystemd.
//!
//! Talks to a running [`rystemd`] manager over its JSON-line control transport
//! control channel (via `rystemd::client::Client`). The daemon itself lives
//! in the `rystemd` crate; this crate is purely the control surface.

pub mod cli;
