# Handbook

Worked examples for writing units and driving the manager with `rustemctl`.
On Linux, `rustemctl` may be symlinked as `systemctl`; the examples use that
compatibility name.

Linux examples assume `./target/release/rustemd daemon --user` is running.
Windows examples use either `rustemd.exe daemon --user` or the native SCM host.

---

## 1. A basic service

`~/.config/systemd/user/hello.service` (user mode):

```ini
[Unit]
Description=Hello world service

[Service]
Type=simple
ExecStart=/usr/bin/env sh -c 'while true; do echo hello; sleep 5; done'
Restart=on-failure
```

Start it and watch its captured output:

```sh
systemctl --user start hello
systemctl --user status hello      # shows the log ring
systemctl --user stop hello
```

`Type=simple` goes active immediately after spawning; the unit's **cgroup**
(Linux cgroup v2) is the supervision boundary, so `stop` SIGTERMs (then
SIGKILLs) the whole tree — even children that double-fork out of their process
group.

## 2. A oneshot "state" service

Use `RemainAfterExit=yes` when a service represents *state* rather than a
long-running process:

```ini
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/bin/touch /tmp/rustemd-flag
ExecStop=/usr/bin/rm -f /tmp/rustemd-flag
```

```sh
systemctl --user start flag.service
systemctl --user is-active flag.service   # -> active
systemctl --user stop flag.service        # runs ExecStop
```

Without `RemainAfterExit=yes`, a oneshot goes `inactive(dead)` after it runs —
exactly like systemd.

## 3. A forking daemon (PIDFile)

```ini
[Service]
Type=forking
ExecStart=/usr/sbin/mydaemon
PIDFile=/run/mydaemon.pid
```

The manager waits for the `ExecStart` parent to exit, then reads `PIDFile` to
find the surviving child. (`PIDFile` names the main pid; the cgroup still
tracks the full tree for cleanup.)

## 4. A timer

Timers are separate units that trigger a matching service unit.

`~/.config/systemd/user/daily-backup.timer`:

```ini
[Timer]
OnCalendar=*-*-* 03:00:00
Persistent=yes

[Install]
WantedBy=timers.target
```

`daily-backup.service` (same base name, so the timer finds it automatically):

```ini
[Service]
Type=oneshot
ExecStart=/usr/local/bin/backup.sh
```

```sh
systemctl --user start daily-backup.timer
systemctl --user list-timers
```

The calendar engine accepts the full systemd grammar — `daily`, `weekly`,
`Mon..Fri 09:00`, `*:0/15` (every 15 minutes), `2026-08-21 09:00` (a one-shot
date), lists and steps (`Mon,Wed 09:00/2`). Monotonic forms
(`OnBootSec=5min`, `OnUnitActiveSec=1h`) work too.

## 5. Enabling at boot / login

`enable`/`disable`/`is-enabled` mirror systemd's `[Install]` symlink model:

```ini
[Install]
WantedBy=default.target
```

```sh
systemctl --user enable hello.service     # creates a .wants symlink
systemctl --user is-enabled hello.service # -> enabled
systemctl --user disable hello.service
```

`RequiredBy=` creates a `.requires` symlink; `Alias=` and `Also=` are honored.

## 6. Dependencies

```ini
[Unit]
Description=Web app
Requires=postgres.service
After=postgres.service

[Service]
ExecStart=/usr/local/bin/webapp
```

`Requires` starts the dependency (and pulls it down if it fails);
`Wants` starts it but ignores failure; `After` only orders, it doesn't imply
a start. `Conflicts` stops the named unit when this one starts.

## 7. Drop-ins and specifiers

Override a stock unit without editing it — put a file in
`hello.service.d/`:

```ini
# ~/.config/systemd/user/hello.service.d/override.conf
[Service]
Environment=GREETING=howdy
```

Specifiers expand at load time: `%n` (name), `%p` (prefix before `@`),
`%i` (instance after `@`), `%u`/`%g` (user/group), `%h` (home), `%t` (runtime
dir), `%%` (literal `%`). Instanced units (`web@1.service`) use `%i`.

## 8. Programmatic control (no shell, no D-Bus)

```rust
use rustemd::control::{Control, SocketClient};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // system mode (false) or user mode (true).
    let mut ctl = SocketClient::for_mode(true)?;

    ctl.start(&["hello.service"])?;
    ctl.restart(&["hello.service"])?;

    // Typed, owned status records.
    for s in ctl.status(&["hello.service"])? {
        println!("{}: {}/{}", s.name, s.active, s.sub);
    }

    for t in ctl.list_timers()? {
        println!("timer {} activates {:?}", t.unit, t.next);
    }

    ctl.stop(&["hello.service"])?;
    Ok(())
}
```

Both `Manager` (in-process) and `SocketClient` (remote) implement the same
`Control` trait, so you can write against `&mut dyn Control` and swap the
backend freely. This is the library alternative to `systemctl` or D-Bus.

---

## Filesystem layout

| | system | user |
| --- | --- | --- |
| unit path | `/etc/systemd/system` → `/run/...` → `/usr/lib/...` | `~/.config/systemd/user` → `/etc/systemd/user` → `/usr/lib/systemd/user` |
| `[Install]` dir | `/etc/systemd/system` | `~/.config/systemd/user` |
| runtime | `/run` | `$XDG_RUNTIME_DIR` |

Every path is overridable for tests via `RUSTEMD_UNIT_PATH`,
`RUSTEMD_CONFIG_DIR`, `RUSTEMD_RUNTIME_DIR`, and `RUSTEMD_SOCKET`.

---

## TUI + shell completions

- **TUI** — `rustemd-tui --user` connects to a running manager over the
  `Control` API (it detects the socket; it never starts a second daemon) and
  shows tabbed live views: Units / Services / Timers / Unit files, with a
  status pane and single-key actions (`s` start, `x` stop, `r` restart, …).
- **Completions** — `rustemctl completions <bash|fish|zsh|powershell|nushell>`
  emits a completion script for that shell, named after the invoked binary.



---

## Windows manager

### Per-user mode

Build and start the manager from PowerShell:

```powershell
cargo build --release
.\target\release\rustemd.exe daemon --user
```

Place units in either of these directories (higher precedence first):

- `%LOCALAPPDATA%\rustemd\config`
- `%LOCALAPPDATA%\rustemd\units`

Then use the normal client from another terminal:

```powershell
.\target\release\rustemctl.exe --user daemon-reload
.\target\release\rustemctl.exe --user start hello.service
.\target\release\rustemctl.exe --user status hello.service
```

A minimal Windows service unit uses native Windows command-line programs:

```ini
[Unit]
Description=Windows worker

[Service]
Type=simple
ExecStart=C:\Tools\worker.exe --serve
Restart=on-failure

[Install]
WantedBy=default.target
```

The process and all descendants run in a Win32 Job Object. `stop` terminates
the Job Object, and manager exit closes every remaining job.

### SCM system mode

From an elevated terminal:

```powershell
rustemd.exe service install
sc.exe start rustemd
rustemctl.exe list-units
sc.exe stop rustemd
rustemd.exe service uninstall
```

Use `service install --manual` for demand start, or `--name` and
`--display-name` for a custom registration. System units are searched in:

- `%ProgramData%\rustemd\config`
- `%ProgramData%\rustemd\units`

SCM stop and system-shutdown controls request an orderly manager shutdown;
the control callback itself does not run unit lifecycle work.

### TCP socket trigger

```ini
# api.socket
[Socket]
ListenStream=127.0.0.1:8080
Service=api.service

# api.service
[Service]
Type=simple
ExecStart=C:\Tools\api.exe
```

Starting `api.socket` binds the TCP listener. A pending connection is accepted as the trigger and activates `api.service`.
For this MVP the listener remains owned by rustemd and is not
passed to the child, so this is launch-on-connection rather than full systemd
`LISTEN_FDS` handoff. Unix-domain listeners are not supported on Windows.

### Windows compatibility table

| Capability | Windows MVP |
| --- | --- |
| `Type=simple`, `exec`, `idle`, `oneshot` | Supported |
| `.timer`, `.target` | Supported |
| TCP `.socket` trigger | Supported; no child socket handoff |
| `MemoryMax=`, `TasksMax=` | Win32 Job Object limits |
| `Type=forking`, `notify`, `dbus` | Explicit error |
| `User=`, `Group=` | Explicit error |
| `MemoryHigh=`, `CPUWeight=`, `KillMode=process` | Explicit error |
| Unix sockets, cgroups, mounts, devices, boot | Linux only |
