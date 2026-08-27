# Compatibility & known issues

rystemd understands the systemd unit language and lifecycle, but it is a
reimplementation — a few areas are deliberately partial. The full, current
matrix lives in [`KNOWN_ISSUES.md`](https://github.com/fralondale/rustemd/blob/main/KNOWN_ISSUES.md); the summary below is the short version.

## Unit types

Supported: `.service`, `.socket`, `.timer`, `.target`, `.mount`, `.path`,
`.device`.

Not yet supported:

- `.slice` / `.scope` — the cgroup hierarchy (`user.slice`, `app.slice`,
  `session.slice`);
- `.automount`;

These are the main gap for a strict drop-in on a modern desktop, which leans
heavily on slices.

## Sandbox directives

Many hardening directives are parsed but not (yet) enforced, and load with a
warning rather than failing: `SystemCallFilter`, `ProtectKernelModules`,
`MemoryDenyWriteExecute`, `DeviceAllow`, `RestrictNamespaces`, and others.
Services that rely on them run but without that particular isolation. Enforced
now (Linux/x86_64): `SystemCallFilter=`, `PrivateDevices=`,
`RestrictRealtime=`, `LockPersonality=`, `RestrictSUIDSGID=`, and
`RestrictAddressFamilies=`.

## Condition / assertion support

A working set of `Condition*=`. A few less-common kinds
(`ConditionVirtualization`, `ConditionNeedsUpdate`, `ConditionSecurity`,
`ConditionPathIsManifest`, `ConditionKernelCommandLine`, `ConditionCredential`)
are parsed but always-false/always-true, so units keyed on them will either
always or never load.

## Timeouts & limits

`TimeoutStopFailureMode=` (used widely on Fedora), `TimeoutSec=` and a few
`LimitEXT*` relaxations are not fully honored. Some desktop services
(`user@1000.service`) are heavy on `PAMName`, `KeyringMode`, `Delegate`,
`ImportCredential`, and `RequiresMountsFor=` which are not yet implemented.

## Restart / readiness

`Restart=` policies (`always`, `on-failure`, ...) are implemented; the
trigger-rate limit uses a fixed 5-in-10s window.

## Battery

These summary bullets are intentionally short. For the complete, version-
specific matrix — every directive, its status, and which real Fedora units it
affects — see KNOWN_ISSUES.md and the tiered boot-compatibility notes.