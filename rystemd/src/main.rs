//! `rystemd` — the unit-manager binary (PID 1 on Linux or SCM-hosted on Windows).

fn main() {
    std::process::exit(rystemd::daemon::entry());
}
