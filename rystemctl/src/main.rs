//! `rystemctl` — the `systemctl`-compatible control CLI.

fn main() {
    // Restore the default SIGPIPE disposition. Rust's runtime sets SIGPIPE to
    // SIG_IGN, which turns EPIPE on a `println!` into a panic when a consumer
    // closes the pipe early (`rystemctl list-units | head`). systemctl dies
    // silently in that case; match it. Unix-only (no SIGPIPE on Windows).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    std::process::exit(rystemctl::cli::entry());
}
