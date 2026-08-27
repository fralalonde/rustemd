# Windows port

Windows is supported as a compatibility port of the manager, CLI, and TUI —
systemd-like semantics backed by Win32 primitives. Linux is the primary
target; the Windows port exists so the manager + `rystemctl`/`rystemd-tui`
stack runs there too.

## Modes

### Per-user mode

```powershell
cargo build --release
.\target\release\rystemd.exe daemon --user
```

Unit search order (higher precedence first):

- `%LOCALAPPDATA%\rystemd\config`
- `%LOCALAPPDATA%\rystemd\units`

```powershell
.\target\release\rystemctl.exe --user daemon-reload
.\target\release\rystemctl.exe --user start hello.service
.\target\release\rystemctl.exe --user status hello.service
```

A minimal service unit uses native Windows programs:

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

The process and its descendants run in a Win32 **Job Object**; `stop`
terminates the Job Object, and manager exit closes every remaining job.

## SCM system mode

From an elevated terminal:

```powershell
rystemd.exe service install
sc.exe start rystemd
rystemctl.exe list-units
sc.exe stop rystemd
rystemd.exe service uninstall
```

`service install --manual` for demand start; `--name` / `--display-name` for a
custom registration. System units live under:

- `%ProgramData%\rystemd\config`
- `%ProgramData%\rystemd\units`

## TCP socket trigger

```ini
# api.socket
[Socket]
ListenStream=127.0.0.1:8080
Service=api.service
```

Starting `api.socket` binds the TCP listener; a pending connection activates
`api.service`. The listener stays owned by rystemd for this launch-on-connection
model (no `LISTEN_FDS` handoff on Windows). Unix-domain listeners are not
supported on Windows.

## Windows compatibility table

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