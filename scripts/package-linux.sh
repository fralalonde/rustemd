#!/usr/bin/env bash
# Package rystemd for Linux: a portable tar.gz, a .deb, and a .rpm.
# The release binaries must already be built.
#
# Usage: scripts/package-linux.sh <target-triple> <version>
#   e.g.  scripts/package-linux.sh x86_64-unknown-linux-gnu 0.2.0
set -euo pipefail

TARGET="${1:?target triple required}"
VERSION="${2:?version required}"

case "$TARGET" in
    x86_64-unknown-linux-gnu)  DEB_ARCH=amd64   ; RPM_ARCH=x86_64 ;;
    aarch64-unknown-linux-gnu) DEB_ARCH=arm64   ; RPM_ARCH=aarch64 ;;
    *) echo "unsupported target: $TARGET" >&2; exit 2 ;;
esac

# Absolute — rpmbuild changes cwd into its BUILD dir, so a relative path
# would break inside %install.
BIN_DIR="$(pwd)/target/$TARGET/release"
DIST="target/dist"
mkdir -p "$DIST"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# --- portable tar.gz -------------------------------------------------------
PKG_ROOT="$STAGE/rystemd-$VERSION-$TARGET"
mkdir -p "$PKG_ROOT/bin" "$PKG_ROOT/completions"
install -m 0755 "$BIN_DIR/rystemd"     "$PKG_ROOT/bin/rystemd"
install -m 0755 "$BIN_DIR/rystemctl"   "$PKG_ROOT/bin/rystemctl"
install -m 0755 "$BIN_DIR/rystemd-tui" "$PKG_ROOT/bin/rystemd-tui"
ln -s rystemctl "$PKG_ROOT/bin/systemctl"
"$BIN_DIR/rystemctl" completions bash       > "$PKG_ROOT/completions/rystemctl.bash"
"$BIN_DIR/rystemctl" completions zsh        > "$PKG_ROOT/completions/_rystemctl"
"$BIN_DIR/rystemctl" completions fish       > "$PKG_ROOT/completions/rystemctl.fish"
"$BIN_DIR/rystemctl" completions powershell > "$PKG_ROOT/completions/rystemctl.ps1"
"$BIN_DIR/rystemctl" completions nushell    > "$PKG_ROOT/completions/rystemctl.nu"
printf '%s\n' "$VERSION" > "$PKG_ROOT/VERSION"
tar -C "$STAGE" -czf "$DIST/rystemd-$VERSION-$TARGET.tar.gz" "rystemd-$VERSION-$TARGET"

# --- .deb ------------------------------------------------------------------
DEB_ROOT="$STAGE/deb"
mkdir -p "$DEB_ROOT/DEBIAN" "$DEB_ROOT/usr/bin" \
    "$DEB_ROOT/usr/share/bash-completion/completions" \
    "$DEB_ROOT/usr/share/zsh/site-functions" \
    "$DEB_ROOT/usr/share/fish/vendor_completions.d"
install -m 0755 "$BIN_DIR/rystemd"     "$DEB_ROOT/usr/bin/rystemd"
install -m 0755 "$BIN_DIR/rystemctl"   "$DEB_ROOT/usr/bin/rystemctl"
install -m 0755 "$BIN_DIR/rystemd-tui" "$DEB_ROOT/usr/bin/rystemd-tui"
install -m 0644 "$PKG_ROOT/completions/rystemctl.bash" "$DEB_ROOT/usr/share/bash-completion/completions/rystemctl"
install -m 0644 "$PKG_ROOT/completions/_rystemctl"     "$DEB_ROOT/usr/share/zsh/site-functions/_rystemctl"
install -m 0644 "$PKG_ROOT/completions/rystemctl.fish" "$DEB_ROOT/usr/share/fish/vendor_completions.d/rystemctl.fish"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@ARCH@/$DEB_ARCH/g" \
    packaging/debian/control > "$DEB_ROOT/DEBIAN/control"
dpkg-deb --build --root-owner-group "$DEB_ROOT" "$DIST/rystemd-${VERSION}-${DEB_ARCH}.deb"

# --- .rpm ------------------------------------------------------------------
RPM_TOP="$STAGE/rpm"
mkdir -p "$RPM_TOP"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
rpmbuild -bb packaging/rystemd.spec \
    --define "_topdir $RPM_TOP" \
    --define "version $VERSION" \
    --define "_bindir_stage $BIN_DIR"
find "$RPM_TOP/RPMS" -name '*.rpm' -exec cp {} "$DIST/" \;

echo "--- dist/ ---"
ls -1 "$DIST"
