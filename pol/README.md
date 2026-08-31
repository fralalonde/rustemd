# SELinux policy for rystemd

This directory holds a [refpolicy](https://github.com/SELinuxProject/refpolicy)
module that gives the rystemd binaries a confined domain (`rystemd_t`) with the
privileges an init/unit-manager needs: bootstrapping, reading unit files,
spawning and supervising child services, managing its own `/var`/`/run` trees,
and (with the `boot` feature) mounting API filesystems and `switch_root`.

## Status — read this first

**The module compiles and packages cleanly against the refpolicy devel headers
(verified with `checkmodule` + `semodule_package`), but it is not yet
live-verified under enforcing SELinux.** The host rystemd is developed on runs
with SELinux **disabled**, so the module can be authoritatively *built* but
**not** *boot-tested* on a real enforcing Fedora host. Do not install it
expecting a turnkey enforcing boot; treat it as the starting point and fold
`audit2allow` output back in.

What still stands between this and a full enforcing rystemd-init boot:

1. **Live iteration.** On a real enforcing host, boot once with rystemd as init,
   collect denials (`ausearch -m avc`), and fold them back as new `allow`
   rules. That feedback loop is the authoritative source of the long tail.
2. **The graphical/desktop stack** (gdm, gnome-shell, logind, dbus-activation)
   is outside this module — those are separate domains that already exist in
   Fedora's policy and need their own supervision rules.
3. **The unit ecosystem** rystemd starts (journald, NetworkManager, …) is not
   listed here; each gets its own `optional_policy` block when packaged.

`rystemd.pp` (the built module) is gitignored — commit the `.te`/`.fc` source
and build it locally.

## Build

On a Fedora host with SELinux enabled and policy-devel installed:

```sh
# install the devel toolchain (one-time)
sudo dnf install selinux-policy-devel

# build & package the module (standard refpolicy devel Makefile)
cd pol
make -f /usr/share/selinux/devel/include/Makefile rystemd.pp
```

Offline/host-independent compile-check (no devel headers on the machine):

```sh
# Extract selinux-policy-devel + checkpolicy + m4 into $PREFIX, then build an
# all_interfaces.conf the way refpolicy's include/Makefile does:
mkdir -p build/tmp && printf 'divert(-1)\n' > build/all_interfaces.conf
m4 -s $PREFIX/usr/share/selinux/devel/include/support/*.spt \
    $(find $PREFIX/usr/share/selinux/devel/include -name '*.if' | sort) \
    >> build/all_interfaces.conf && printf 'divert\n' >> build/all_interfaces.conf

# Then compile the module:
m4 -s $PREFIX/usr/share/selinux/devel/include/support/*.spt \
    build/all_interfaces.conf rystemd.te > build/rystemd.tmp
$PREFIX/usr/bin/checkmodule -m build/rystemd.tmp -o build/rystemd.mod
m4 -s $PREFIX/usr/share/selinux/devel/include/support/*.spt rystemd.fc |
    grep -v '^#' > build/rystemd.mod.fc     # gen_context needs -D enable_mcs
semodule_package -o rystemd.pp -m build/rystemd.mod -f build/rystemd.mod.fc
```

## Install + label

```sh
sudo semodule -i rystemd.pp
sudo restorecon -v /usr/bin/rystemd /usr/bin/rystemctl /usr/bin/rystemd-tui
```

Check the domain the binary will run in:

```sh
secon -t $(readlink -f /usr/bin/rystemd)
```

## Files

- `rystemd.te`  — the policy source (type declarations + allow rules).
- `rystemd.fc`  — file-context labels for the binaries and data trees.
- `rystemd.if`  — exported interfaces for other modules to reference (added as
  soon as a consumer exists).

## Iterating from audit2allow

Build with `audit2allow` on the enforcing host, review the added perms, then
hand-add the safe ones here:

```sh
ausearch -m avc -ts recent | audit2allow -M rystemd-local
grep -E '^allow' rystemd-local.te          # review
```