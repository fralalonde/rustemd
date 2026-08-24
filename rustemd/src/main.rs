//! `rustemd` — the PID-1 init/manager binary.

fn main() {
    std::process::exit(rustemd::daemon::entry());
}
