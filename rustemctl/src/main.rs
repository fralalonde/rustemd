//! `rustemctl` — the `systemctl`-compatible control CLI.

fn main() {
    std::process::exit(rustemctl::cli::entry());
}
