//! `rustemd` — the init/manager binary and the `systemctl`-compatible CLI.

fn main() {
    std::process::exit(rustemd::cli::entry());
}
