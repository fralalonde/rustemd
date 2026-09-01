#!/usr/bin/env bash
# Automates semantic version bumping and tagging in Git.
#
# Usage:
#   ./release.sh <major|minor|patch> [--push] [-y]
#
# Examples:
#   ./release.sh patch
#   ./release.sh minor --push
#   ./release.sh patch -y       # tag and push without prompting
#
# Without --push/-y, the script prompts before pushing. The default answer is no.
set -euo pipefail

show_help() {
    cat <<'EOF'
Automates semantic version bumping and tagging in Git.

Usage:
  release.sh <major|minor|patch> [--push|-y]

Examples:
  ./release.sh patch
  ./release.sh minor --push
  ./release.sh patch -y

--push or -y: tag and push to origin (branch + tag) in one shot, no prompt.
Without it, you are prompted before pushing (default no).
EOF
}

increment=""
push=0
for arg in "$@"; do
    case "$arg" in
        major|minor|patch) increment="$arg" ;;
        --push|-y|--yes) push=1 ;;
        -h|--help) show_help; exit 0 ;;
        *) echo "Unknown argument: $arg" >&2; show_help; exit 1 ;;
    esac
done

if [[ -z "$increment" ]]; then
    show_help
    exit 1
fi

# Sanity gate: the tree must compile before any version/tag manipulation.
echo "→ Running cargo check..."
cargo check

# Get latest tag or default to v0.0.0
latest_tag="$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-version:refname | head -n 1)"
latest_tag="${latest_tag:-v0.0.0}"

# Bump version
version_str="${latest_tag#v}"
IFS=. read -r major minor patch <<<"$version_str"
case "$increment" in
    major) major=$((major + 1)); minor=0; patch=0 ;;
    minor) minor=$((minor + 1)); patch=0 ;;
    patch) patch=$((patch + 1)) ;;
esac

new_tag="v${major}.${minor}.${patch}"
commit_message="Release $new_tag"

branch="$(git branch --show-current)"
if [[ -z "$branch" ]]; then
    echo "Aborted: repository is in detached HEAD state. Check out a branch before releasing." >&2
    exit 1
fi

# Check working directory
if [[ -n "$(git status --porcelain)" ]]; then
    echo "Repository has uncommitted changes:" >&2
    git status
    read -r -p "Add and commit all changes with message '$commit_message'? (y/N) " choice
    if [[ "$choice" =~ ^(y|yes)$ ]]; then
        git add -A
        git commit -m "$commit_message"
    else
        echo "Aborted due to uncommitted changes." >&2
        exit 1
    fi
fi

# Sync the workspace version to the release in every Cargo.toml that declares
# one (the git tag is the source of truth; the manifests just mirror it for the
# toolchain). The workspace root has no `version =`, so the member crates carry
# it — sync each, then keep the lockfile's own-package entries in sync and
# commit only if something actually changed (a no-op bump must not abort).
new_version="${major}.${minor}.${patch}"
for f in Cargo.toml */Cargo.toml; do
    [[ -f "$f" ]] || continue
    if grep -qE '^version = "[0-9]+\.[0-9]+\.[0-9]+"' "$f"; then
        sed -i -E "0,/^version = \"[0-9]+\.[0-9]+\.[0-9]+\"/s//version = \"${new_version}\"/" "$f"
    fi
done
cargo metadata --format-version 1 >/dev/null 2>&1 || true
git add -A
if ! git diff --cached --quiet; then
    git commit -m "$commit_message"
fi

# Tag
git tag -a "$new_tag" -m "$commit_message"
echo "Tagged with $new_tag"

push_release() {
    git push origin "$branch"
    git push origin "$new_tag"
    echo "Pushed branch $branch and tag $new_tag"
    # Regenerate + push the downstream package-manager repos (Homebrew tap,
    # Scoop bucket) so brew/scoop install & update see the new version. Only
    # once the tag is pushed (assets may be uploaded by CI asynchronously, so
    # bumping here is best-effort; a re-run with scripts/bump-package-managers.sh
    # handles a CI hiccup). Skips cleanly if either sibling repo is absent.
    if [[ -x scripts/bump-package-managers.sh ]]; then
        if scripts/bump-package-managers.sh "$new_version" --push; then
            echo "Bumped Homebrew + Scoop for $new_tag"
        else
            echo "Warning: package-manager bump failed (check sibling repos)." >&2
        fi
    fi
}

if [[ "$push" -eq 1 ]]; then
    push_release
else
    read -r -p "Push branch $branch and tag $new_tag to trigger the release pipeline? (y/N) " choice
    if [[ "$choice" =~ ^(y|yes)$ ]]; then
        push_release
    else
        echo "Not pushed. To trigger the release pipeline manually, run:"
        echo "  git push origin $branch"
        echo "  git push origin $new_tag"
    fi
fi
