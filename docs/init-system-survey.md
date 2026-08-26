# Init System Design Survey

A working note on the design decisions of the major init/service systems, and
which of them are worth adopting, adapting, or rejecting for **rystemd**.

**Guiding constraint:** rystemd is a from-scratch reimplementation whose unit
files and `systemctl`-style CLI must stay *drop-in compatible with systemd*.
That means the dependency vocabulary, unit-file syntax, service types, timer
semantics, and `[Install]` behavior are fixed by systemd; the survey is about
what the *other* systems got right (or wrong) that we can learn from without
breaking that contract.

---

## 1. The design axes

Every init system makes choices along a small number of orthogonal axes. This
is the map the rest of the note is organized around.

| Axis | The question | Spectrum |
|---|---|---|
| Configuration | How are services described? | declarative (systemd, dinit, launchd) ↔ executable scripts (sysvinit, OpenRC, BSD rc, runit/s6) |
| Process tracking | How does the manager know a service is alive? | cgroups (systemd) ↔ supervision (runit/s6/dinit) ↔ pidfiles (sysvinit/OpenRC/BSD) |
| Orchestration | Does it manage *ordering* between services, or just keep daemons alive? | dependency graph (systemd/dinit/s6-rc) ↔ supervision only (runit) ↔ script order only (sysvinit) |
| Startup shape | Sequential, parallel, or on-demand? | sequential (sysvinit) ↔ parallel (dinit/s6-rc/systemd) ↔ on-demand (launchd) |
| Runlevels | How are "system states" expressed? | numbered runlevels (sysvinit) ↔ targets (systemd/dinit) ↔ bundles (s6-rc) ↔ none (launchd) |
| Readiness | How does a daemon signal "I'm up"? | sd_notify (systemd) ↔ notifycheck (s6) ↔ notifyfd (dinit) ↔ none (pidfile-era) |
| Control plane | How does the admin talk to PID 1? | D-Bus (systemd) ↔ Mach/launchctl (launchd) ↔ own socket (s6-rc/dinit/rystemd) |

---

## 2. What each system decided

### systemd — the reference
Poettering's "Rethinking PID 1" is the canonical statement of the problem: a
good init starts *less* and starts *more in parallel*.[1] The two mechanisms are
on-demand activation (socket/D-Bus/path/timer) and dependency-driven parallel
startup. Units are declarative INI files (`[Unit]`/`[Service]`/`[Install]`),[2]
and sockets are first-class: a `.socket` unit holds the listening fd and starts
its matching service on first connection.[3] Process tracking is via cgroups so
the kernel — not a pidfile — is the source of truth.[1]

### sysvinit — the baseline
Sequential startup through numbered runlevels (0–6, `S`) and `/etc/rc.d/init.d`
scripts linked in order. Dependency information lives *implicitly* in the
symlink numbering (`S20foo`, `K30bar`), not in the scripts. It is the thing
systemd was reacting against: serial, pidfile-based, shell-bound. (Common
knowledge; kept as the historical baseline rather than a cited source.)

### OpenRC — dependency metadata *inside* shell scripts
Services are still shell scripts, but each declares a `depend()` function with
`need` (hard), `use` (soft — only if present in the runlevel), `want`, `after`,
and `before`.[4][5] Critically, OpenRC *extracts* this metadata by actually
executing the scripts: the `librc-depend.c` source documents a "7 phase
operation" whose first phase is "source all init scripts and print
dependencies" via a forked shell.[4] That is the sharp lesson: embedding
dependency info in executable code forces the manager to run untrusted shell
just to learn the graph.

### runit — pure supervision, no dependencies
A three-stage lifecycle (`/etc/runit/1` → `2` → `3`),[12] where stage 2 runs
`runsvdir`, which supervises one `runsv` per service directory symlinked into
the runsvdir.[13] Enabling a service = creating a symlink; a `down` file marks
it administratively stopped.[13] There is deliberately *no* dependency
management — daemons are launched in parallel and simply crash-and-restart
until their implicit dependencies happen to be up.

### s6-rc — supervision *plus* an offline-compiled dependency graph
s6 (the supervision layer) and s6-rc (the service manager) are separated. s6-rc
distinguishes **longruns** (supervised daemons) from **oneshots** (state changes
with no process — e.g. mounting a filesystem) and **bundles** (named sets of
services, its answer to runlevels).[6] Its signature idea is the *offline
compile*: dependency analysis and graph validation happen at config-build time,
never at boot, because "boot time is the worst possible time to detect
errors".[8] The `why` page is also explicit that pure supervision is
*insufficient* for complex systems — "supervision suites do not perform
dependency management."[7]

### dinit — the clean-room middle ground
Declarative service files, dependency-based parallel startup, and supervision;
Chimera Linux describes it as "dependency-based (unlike runit), supervising
(unlike sysvinit), and portable (unlike systemd)".[10][11] Service types
(`process`, `bgprocess`, `scripted`/oneshot, `internal`, `triggered`) map
almost one-to-one onto systemd's.[9] It distinguishes a hard *depends-on* link
from a *waits-for* ordering link that carries no lifecycle dependency.[11]
Runs as either PID 1 or a user manager.[10]

### FreeBSD rc — ordering keywords, deliberately weak guarantees
`rcorder(8)` reads `# PROVIDE:`/`# REQUIRE:`/`# BEFORE:`/`# KEYWORD:` comments
out of shell scripts and emits a topologically sorted order.[14] The docs are
candid that `REQUIRE:` "does not guarantee that the service will actually be
running" — it only orders the scripts; the application must cope with a missing
prerequisite.[14] Shutdown runs the same scripts in reverse order.

### launchd — the on-demand extreme
Everything is a property-list file; `launchd` is simultaneously PID 1, the
service manager, and the socket/event activator.[15][16] On-demand launch is the
default posture, triggered by sockets, file-system paths, Mach messages, and
timers.[15] It demands services not fork/daemonize ("if a process goes into the
background, launchd will lose track of it").[16] Its socket-activation protocol
differs from systemd's: socket *names* are hardcoded in the app, versus
systemd's fixed fd 3.[16]

---

## 3. Salient decisions → rystemd

### 3.1 Adopt (already have, or should)

**Declarative unit files, never executable scripts for metadata.**
rystemd already parses INI unit files. OpenRC/BSD prove the failure mode of the
alternative: extracting `depend()`/`PROVIDE` metadata requires *executing* shell,
which is slow, fragile, and runs untrusted code just to build the graph.[4][14]
This is the single strongest argument for rystemd's declarative design.

**longrun vs oneshot as the fundamental split.**
s6-rc's "a service is either a longrun or a oneshot"[6] is the cleanest framing
of what systemd expresses as `Type=simple/forking/notify` vs `Type=oneshot`.
rystemd already has both; making the distinction *conceptual* (a oneshot is a
state change, not a process to supervise) rather than incidental is worth
reflecting in the docs.

**`depends-on` vs `waits-for` = `Requires` vs `After`.**
dinit's hard link vs ordering-only link[11] and FreeBSD's honest "REQUIRE only
orders, app must cope"[14] are both just systemd's `Requires=` vs `After=`.[2]
No new vocabulary — but it confirms the value of keeping the *ordering* path
strictly separate from the *lifecycle* path, which rystemd's dep engine does.

**Offline (or at least out-of-boot) graph validation.**
s6-rc's refusal to validate the graph at boot[8] maps to a concrete rystemd
rule: unit-load/reload must validate the graph and *fail loudly at load time*,
never panic or half-boot at PID-1 time. This is a discipline, not a feature.

**Targets, not runlevels.**
s6-rc's bundles and dinit's `.target` sentinels are functionally systemd's
targets.[6][11] rystemd already uses targets. Runlevels are a dead end.

**Supervision contract: "don't double-fork."**
launchd's requirement that daemons stay in the foreground[16] is shared by
runit, s6, dinit, and systemd. rystemd's process-group tracking is the cgroups
stand-in, so the same rule applies: a self-daemonizing service escapes the
process group. Worth documenting as an explicit, enforced contract.

**On-demand activation is the biggest remaining systemd gap.**
"Start less, start more in parallel"[1] is realized through socket/path/timer
activation. rystemd has timers; `.socket` (and eventually `.path`) is the
natural next step, and launchd shows how much an init can lean on the on-demand
posture.[15] Socket activation is also *plumbable into systemd-compat*: the
`.socket` unit format and fd-passing protocol are fixed by systemd,[3] so it is
an additive, compatible feature.

### 3.2 Reject

**Runlevels.** Replaced by targets everywhere that matters (systemd, dinit,
s6-rc). No reason to re-introduce numbered states.

**Pidfiles as the tracking mechanism.** OpenRC/BSD/sysvinit use them; systemd
and every supervision suite abandoned them for good reason (stale files, PID
reuse). rystemd keeps pidfile *parsing* for `Type=forking` compatibility, but
process groups remain the tracking mechanism — never trust a pidfile for state.

**Pure supervision without dependencies.** s6-rc's own rationale document rules
this out for general-purpose systems.[7] rystemd keeps the dependency graph.

**Sequential startup.** The one thing every post-sysvinit system rejected.[1]

**Shell in the hot path.** OpenRC/BSD run everything through `sh`; rystemd
parses argv itself and only invokes a shell when the unit literally says
`sh -c`. Keep it that way.

### 3.3 Deliberate divergences (allowed, because they don't break compat)

**Control plane.** systemd talks D-Bus; launchd talks Mach; s6-rc/dinit/rystemd
talk a private unix socket. rystemd's JSON-over-unix-socket IPC is a control-
plane choice that never surfaces in a unit file, so it does not affect drop-in
compatibility. Keep it, and keep the `systemctl`-compatible CLI as the
compatibility surface.

**Readiness protocol.** systemd's `sd_notify` is the wire format rystemd already
implements. s6's `s6-notifyoncheck` (probe-based readiness)[6] and dinit's
notifyfd are *ideas* to consider as an additive `Type=notify`-adjacent option,
but the on-disk format stays systemd's.

---

## 4. Recommendations (prioritized)

1. **Document the longrun/oneshot conceptual split** in the handbook — it is the
   clearest mental model for why `Type=` exists.
2. **Enforce load-time graph validation** with loud, non-panicking errors (the
   s6-rc "never at boot" discipline).[8]
3. **Next feature after current work: `.socket` units** — the highest-leverage
   systemd feature rystemd lacks, and a pure additive win for compat.[3]
4. **Document and (where possible) enforce "no self-daemonization"** as the
   supervision contract, since process groups are the cgroups stand-in.[16]
5. **Resist every script-embedded-metadata temptation** — OpenRC/BSD are the
   cautionary tale.[4][14]
6. **Keep the control plane out of unit-file semantics** so the JSON socket can
   evolve without touching compatibility.

---

## Sources

[1] https://0pointer.de/blog/projects/systemd.html
[2] https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html
[3] https://www.freedesktop.org/software/systemd/man/latest/systemd.socket.html
[4] https://github.com/OpenRC/openrc/blob/master/service-script-guide.md
[5] https://github.com/OpenRC/openrc/blob/master/man/openrc-run.8
[6] https://skarnet.org/software/s6-rc/overview.html
[7] https://skarnet.org/software/s6-rc/why.html
[8] https://skarnet.com/projects/s6/rc/concepts.html
[9] https://github.com/davmac314/dinit/blob/master/doc/DESIGN
[10] https://github.com/davmac314/dinit
[11] https://chimera-linux.org/docs/configuration/services
[12] https://man.voidlinux.org/runit.8
[13] https://docs.voidlinux.org/config/services
[14] https://docs.freebsd.org/en/articles/rc-scripting
[15] https://developer.apple.com/library/archive/technotes/tn2083/_index.html
[16] https://en.wikipedia.org/wiki/Launchd
