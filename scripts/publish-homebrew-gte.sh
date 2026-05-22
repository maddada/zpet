#!/usr/bin/env bash
set -euo pipefail

# CDXC:GteHomebrew 2026-05-23-02:59:
# Publishing gte to Homebrew needs the same release, formula, tap-push, and local-install steps each time. Keep that workflow in one guarded script so brew updates are fast without publishing dirty worktrees or mismatched binary versions.

usage() {
  cat <<'USAGE'
Publish the current gte version to GitHub/Homebrew and install it locally.

Usage:
  scripts/publish-homebrew-gte.sh [--yes] [--no-install] [--skip-build]

What it does:
  1. Reads the current version from Cargo.toml.
  2. Requires a clean gte worktree so the release is reproducible.
  3. Creates/pushes gte-v<VERSION> for the current HEAD if needed.
  4. Builds gte and gte-ssh-image release binaries.
  5. Uploads the Apple Silicon tarball to maddada/zpet.
  6. Updates maddada/tap Formula/gte.rb with the new URL and checksum.
  7. Pushes the tap commit.
  8. Runs brew upgrade/install and brew test locally.

Options:
  --yes          Do not prompt before pushing release/tap changes.
  --no-install   Publish only; do not upgrade/install locally with Homebrew.
  --skip-build   Reuse target/release/gte and target/release/gte-ssh-image.
  -h, --help     Show this help.
USAGE
}

YES=0
INSTALL_LOCAL=1
BUILD=1

while [ "$#" -gt 0 ]; do
  case "$1" in
    --yes)
      YES=1
      ;;
    --no-install)
      INSTALL_LOCAL=0
      ;;
    --skip-build)
      BUILD=0
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

log() {
  printf '\033[0;34m[gte-homebrew]\033[0m %s\n' "$*"
}

fail() {
  printf '\033[0;31m[gte-homebrew]\033[0m %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

confirm() {
  if [ "$YES" -eq 1 ]; then
    return 0
  fi
  printf '%s [y/N] ' "$1"
  read -r reply
  [ "$reply" = "y" ] || [ "$reply" = "Y" ]
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$repo_root" ] || fail "Run this from inside the gte git repository."
cd "$repo_root"

require_cmd cargo
require_cmd gh
require_cmd git
require_cmd python3
require_cmd shasum
require_cmd tar
require_cmd brew

[ -f Cargo.toml ] || fail "Cargo.toml not found at repo root."
[ -d crates/fresh-editor ] || fail "This does not look like the gte workspace."

if ! git diff --quiet || ! git diff --cached --quiet; then
  fail "gte worktree has uncommitted changes. Commit the release first, then rerun this script."
fi

version="$(
  awk '
    $0 == "[workspace.package]" { in_workspace = 1; next }
    /^\[/ && in_workspace { exit }
    in_workspace && $1 == "version" {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' Cargo.toml
)"
[ -n "$version" ] || fail "Could not read [workspace.package] version from Cargo.toml."

tag="gte-v${version}"
archive_dir="gte-${version}-aarch64-apple-darwin"
archive_name="${archive_dir}.tar.gz"
archive_path="dist/${archive_name}"
github_repo="maddada/zpet"
git_remote="zpet"
tap_name="maddada/tap"
formula_relpath="Formula/gte.rb"

if ! git remote get-url "$git_remote" >/dev/null 2>&1; then
  fail "Missing git remote '$git_remote'. Expected maddada/zpet as the release remote."
fi

head_sha="$(git rev-parse HEAD)"
needs_tag=0
if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
  tag_sha="$(git rev-list -n 1 "$tag")"
  [ "$tag_sha" = "$head_sha" ] || fail "Tag ${tag} exists but does not point at HEAD."
else
  needs_tag=1
fi

if [ "$BUILD" -eq 1 ]; then
  log "Building release binaries for ${version}"
  cargo build --release -p gte --bin gte --bin gte-ssh-image
fi

[ -x target/release/gte ] || fail "target/release/gte is missing. Run without --skip-build."
[ -x target/release/gte-ssh-image ] || fail "target/release/gte-ssh-image is missing. Run without --skip-build."

built_version="$(target/release/gte --version | awk '{print $2}')"
[ "$built_version" = "$version" ] || fail "Built gte reports ${built_version}, expected ${version}."

log "Packaging ${archive_name}"
rm -rf "dist/${archive_dir}"
mkdir -p "dist/${archive_dir}"
install -m 0755 target/release/gte "dist/${archive_dir}/gte"
install -m 0755 target/release/gte-ssh-image "dist/${archive_dir}/gte-ssh-image"
(cd dist && tar -czf "$archive_name" "$archive_dir")
sha256="$(shasum -a 256 "$archive_path" | awk '{print $1}')"

notes_file="$(mktemp)"
awk -v ver="$version" '
  $0 == "## " ver { found = 1 }
  found && /^## / && $0 != "## " ver { exit }
  found { print }
' CHANGELOG.md > "$notes_file"
if [ ! -s "$notes_file" ]; then
  printf '## %s\n\nRelease %s\n' "$version" "$version" > "$notes_file"
fi

cat <<EOF

Ready to publish:
  Version:      ${version}
  Tag:          ${tag}
  Commit:       ${head_sha}
  Release repo: ${github_repo}
  Asset:        ${archive_path}
  SHA256:       ${sha256}
  Tap:          ${tap_name}

EOF

if [ "$INSTALL_LOCAL" -eq 1 ]; then
  install_prompt="install locally"
else
  install_prompt="skip local install"
fi

if ! confirm "Publish ${tag}, update Homebrew, and ${install_prompt}?"; then
  fail "Aborted."
fi

log "Pushing source branch and tag"
if [ "$needs_tag" -eq 1 ]; then
  log "Creating local tag ${tag}"
  git tag -a "$tag" -m "gte ${version}"
fi
git push "$git_remote" HEAD:main
git push "$git_remote" HEAD:single-file-terminal-editor
git push "$git_remote" "$tag"

if gh release view "$tag" --repo "$github_repo" >/dev/null 2>&1; then
  log "Updating existing GitHub release ${tag}"
  gh release edit "$tag" --repo "$github_repo" --notes-file "$notes_file"
  gh release upload "$tag" "$archive_path" --repo "$github_repo" --clobber
else
  log "Creating GitHub release ${tag}"
  gh release create "$tag" "$archive_path" \
    --repo "$github_repo" \
    --target "$head_sha" \
    --title "gte ${version}" \
    --notes-file "$notes_file"
fi

tap_dir="$(brew --repository "$tap_name")"
formula_path="${tap_dir}/${formula_relpath}"
[ -f "$formula_path" ] || fail "Formula not found: ${formula_path}"

log "Updating ${formula_path}"
python3 - "$formula_path" "$version" "$sha256" <<'PY'
from pathlib import Path
import re
import sys

path = Path(sys.argv[1])
version = sys.argv[2]
sha256 = sys.argv[3]
text = path.read_text()
text = re.sub(
    r'https://github\.com/maddada/zpet/releases/download/gte-v[^/]+/gte-[^-]+-aarch64-apple-darwin\.tar\.gz',
    f'https://github.com/maddada/zpet/releases/download/gte-v{version}/gte-{version}-aarch64-apple-darwin.tar.gz',
    text,
)
text = re.sub(r'sha256 "[0-9a-f]{64}"', f'sha256 "{sha256}"', text, count=1)
text = re.sub(r'The 0\.3\.\d+ artifact includes', f'The {version} artifact includes', text)
path.write_text(text)
PY

log "Validating formula fetch"
brew fetch --force --formula "$tap_name/gte" >/dev/null

if ! git -C "$tap_dir" diff --quiet -- "$formula_relpath"; then
  log "Committing tap formula"
  git -C "$tap_dir" add "$formula_relpath"
  git -C "$tap_dir" commit -m "Update gte to ${version}"
  git -C "$tap_dir" push origin HEAD
else
  log "Tap formula already matches ${version}; no tap commit needed"
fi

if [ "$INSTALL_LOCAL" -eq 1 ]; then
  log "Installing/upgrading local Homebrew gte"
  if brew list --formula "$tap_name/gte" >/dev/null 2>&1 || brew list --formula gte >/dev/null 2>&1; then
    brew upgrade "$tap_name/gte" || brew reinstall "$tap_name/gte"
  else
    brew install "$tap_name/gte"
  fi
  brew test "$tap_name/gte"
  installed_version="$(gte --version | awk '{print $2}')"
  [ "$installed_version" = "$version" ] || fail "Installed gte reports ${installed_version}, expected ${version}."
  log "Installed gte ${installed_version}"
fi

log "Done: ${tag} is published to Homebrew."
