# Interactive live VM

The live VM boots rystemd as PID 1 in QEMU and exposes a serial getty. The
image contains demo units for the supported unit types.

## Run

```sh
scripts/live-vm.sh
```

The script fetches and verifies the pinned Fedora kernel when no kernel path is
provided, builds the reduced live initramfs, and starts QEMU in interactive
serial mode. The host boot image remains unchanged.

Inside the guest:

```sh
rystemctl list-units
rystemctl status demo.service demo.mount demo.socket demo.timer demo.target
rystemctl start demo.mount
rystemctl is-active demo.mount
rystemctl list-timers
printf 'hi\n' | nc 127.0.0.1 8080
rystemctl status demo-echo.service
rystemctl poweroff
```

`rystemctl poweroff` is the clean shutdown path. The minimal guest does not
provide the standard host `shutdown`, `halt`, or `poweroff` commands.

## Demo units

Files live under `examples/live/`.

| Unit | Type | Coverage |
| --- | --- | --- |
| `demo.service` | service | Oneshot execution and `RemainAfterExit` |
| `demo.timer` | timer | `OnBootSec` and `OnUnitInactiveSec` |
| `demo-tick.service` | service | Timer-triggered oneshot |
| `demo.socket` | socket | TCP socket activation on port 8080 |
| `demo-echo.service` | service | Activated service |
| `demo.mount` | mount | Linux tmpfs mount and unmount |
| `demo.target` | target | `Wants=` and `After=` grouping |
| Runtime `.device` units | device | Sysfs enumeration and uevents |

`demo.target` is the default target in the live image. Runtime device units do
not have unit files.

## TUI

The TUI uses the same control socket as the CLI:

```sh
rystemd-tui
```

The serial init sets an 80 by 24 terminal size. The TUI also has a fixed-size
fallback when the terminal reports zero dimensions. Press `q` to exit.

## Automated coverage

The demo unit lifecycle is covered by:

```sh
cargo test -p rystemd --test e2e
```

Mount tests require real mount privileges. Without those privileges, the mount
case self-skips. The VM itself is exercised by `scripts/vm-test.sh`.
