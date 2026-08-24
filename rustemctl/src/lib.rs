//! `rustemctl` — the `systemctl`-compatible control CLI for rustemd.
//!
//! Talks to a running [`rustemd`] manager over its JSON-over-unix-socket
//! control channel (via `rustemd::client::Client`). The daemon itself lives
//! in the `rustemd` crate; this crate is purely the control surface.

pub mod cli;
