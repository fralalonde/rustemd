#!/usr/bin/env bash
# Regenerate the downstream package-manager repos after a rystemd release so
# `brew install` and `scoop install` pick up the new version. Run this AFTER
# release.sh has tagged and pushed (and CI has uploaded the release assets),
# because the formula/manifest pin sha256 hashes computed from those assets.
#
# Handles the two repos that hold version endpoints Homebrew/Scoop read from:
#   - homebrew-rystemd: Formula/rystemd.rb  (version + Linux tarball sha256)
#   - scoop-rystemd:    bucket/rystemd.json (version + Windows zip sha256)
#
# NuGet needs no bump here — release.yml publishes the .nupkg to nuget.org
# directly (and skips cleanly when NUGET_API_KEY is absent). deb/rpm/tarball
# are pure release assets with no external repo to regenerate.
#
# Usage: scripts/bump-package-managers.sh <version> [--push]
#   e.g.  scripts/bump-package-managers.sh 0.2.1 --push
#
# Works against sibling clones (recommended): they must be at ../homebrew-rystemd
# and ../scoop-rystemd relative to this repo, or set HOMEBREW_TAP / SCOOP_BUCKET
# to override. Without --push it regenerates locally and reports; nothing is
# pushed. Requires: bash, git, curl, sha256sum, Python 3.
set -euo pipefail

VERSION="${1:?version required (e.g. 0.2.1)}"
PUSH=0
case "${2:-}" in
    --push|-y|--yes) PUSH=1 ;;
    "") ;;
    *) echo "Unknown argument: $2" >&2; exit 1 ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBREW_TAP="${HOMEBREW_TAP:-$ROOT/../homebrew-rystemd}"
SCOOP_BUCKET="${SCOOP_BUCKET:-$ROOT/../scoop-rystemd}"
ASSET_BASE="https://github.com/rystemd/rystemd/releases/download/v${VERSION}"
sha256_of() { # fetch an asset, retrying until the release has uploaded it (a
              # fresh tag's assets appear asynchronously after CI builds them)
    local url="$1"
    local tries="${2:-60}" delay="${3:-10}"  # ~10 min max by default
    local out
    for _ in $(seq "$tries"); do
        if out="$(curl -fsSL "$url" 2>/dev/null | sha256sum 2>/dev/null)"; then
            echo "$out" | cut -d' ' -f1
            return 0
        fi
        sleep "$delay"
    done
    echo "error: asset not available after ${tries}x${delay}s: $url" >&2
    return 1
}

commit_and_push() { # $1=repo, $2=msg, rest=paths
    local repo="$1" msg="$2"; shift 2
    git -C "$repo" add "$@"
    if ! git -C "$repo" diff --cached --quiet; then
        git -C "$repo" -c user.name="fralalonde" \
            -c user.email="fralalonde@users.noreply.github.com" \
            commit -q -m "$msg"
        echo "committed: $repo ($msg)"
        if [ "$PUSH" -eq 1 ]; then
            git -C "$repo" push -q origin HEAD
            echo "pushed: $repo"
        fi
    else
        echo "no change: $repo"
    fi
}

if [ -d "$HOMEBREW_TAP/.git" ]; then
    # gen-brew-formula.sh lives inside the tap itself; run it in place.
    if [ -x "$HOMEBREW_TAP/scripts/gen-brew-formula.sh" ]; then
        # A fresh tag's assets upload asynchronously after CI builds them, so
        # wait for the Linux tarball (via sha256_of's retry) before running
        # gen-brew-formula.sh — otherwise its curl -fsSL 404s and set -e
        # aborts the whole script before Scoop is even reached.
        sha256_of "$ASSET_BASE/rystemd-$VERSION-x86_64-unknown-linux-gnu.tar.gz" >/dev/null
        ( cd "$HOMEBREW_TAP" && git checkout -q main && git pull -q --ff-only 2>/dev/null || true
          bash scripts/gen-brew-formula.sh "$VERSION" )
        commit_and_push "$HOMEBREW_TAP" "brew: bump rystemd to v${VERSION}" Formula/rystemd.rb
    else
        echo "warning: $HOMEBREW_TAP/scripts/gen-brew-formula.sh missing; skipping brew" >&2
    fi
else
    echo "warning: no homebrew tap clone at $HOMEBREW_TAP; skipping brew" >&2
fi

if [ -d "$SCOOP_BUCKET/.git" ]; then
    cd "$SCOOP_BUCKET"
    git checkout -q main && git pull -q --ff-only 2>/dev/null || true
    zip_sha="$(sha256_of "$ASSET_BASE/rystemd-$VERSION-x86_64-pc-windows-msvc.zip")"
    python3 - "$VERSION" "$zip_sha" <<'PY'
import json, pathlib, sys
ver, sha = sys.argv[1], sys.argv[2]
p = pathlib.Path("bucket/rystemd.json")
m = json.loads(p.read_text())
m["version"] = ver
a = m["architecture"]["64bit"]
a["url"] = f"https://github.com/rystemd/rystemd/releases/download/v{ver}/rystemd-{ver}-x86_64-pc-windows-msvc.zip"
a["hash"] = sha
p.write_text(json.dumps(m, indent=2) + "\n")
PY
    commit_and_push "$SCOOP_BUCKET" "scoop: bump rystemd to v${VERSION}" bucket/rystemd.json
else
    echo "warning: no scoop bucket clone at $SCOOP_BUCKET; skipping scoop" >&2
fi

echo
if [ "$PUSH" -eq 1 ]; then
    echo "Package-manager repos updated and pushed for v$VERSION."
else
    echo "Package-manager repos regenerated for v$VERSION (not pushed)."
    echo "Re-run with --push to push them."
fi