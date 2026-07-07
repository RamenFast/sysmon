#!/usr/bin/env bash
# The release law, executable: every commit that lands on main is a
# release. Run this ON main, AFTER the --no-ff merge of a branch that
# bumped [workspace.package].version.
#
#   scripts/release.sh <notes-file.md>
#
# It refuses to run on a dirty tree, off main, on an already-tagged
# version, or with a version that doesn't match the built binary.
# Then: tag → push → deb + rpm + source tarball + SHA256SUMS →
# GitHub release (marked Latest) → prints the local install command.
set -euo pipefail

notes_file="${1:?usage: scripts/release.sh <notes-file.md>}"
[ -s "$notes_file" ] || { echo "notes file '$notes_file' is missing or empty" >&2; exit 3; }

project_directory="$(cd "$(dirname "$0")/.." && pwd)"
cd "$project_directory"

say()  { printf '\033[36m» %s\033[0m\n' "$*"; }
fail() { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

branch="$(git branch --show-current)"
[ "$branch" = "main" ] || fail "on '$branch' — releases cut from main only"
[ -z "$(git status --porcelain)" ] || fail "working tree is dirty; commit or stash first"

say "build + version law check"
cargo build --release --quiet -p sysmon-app
version="$(target/release/sysmon --version | awk '{print $2}')"
manifest_version="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
[ "$version" = "$manifest_version" ] || fail "binary $version != workspace $manifest_version"
git rev-parse "v$version" >/dev/null 2>&1 && fail "v$version already tagged — bump the version"

say "test suite"
cargo test --release --quiet 2>&1 | tail -2

say "tag v$version + push"
git tag -a "v$version" -m "SysMon $version"
git push -q origin main "v$version"

say "artifacts"
rm -f packaging/dist/*
packaging/build-deb.sh >/dev/null
packaging/build-rpm.sh >/dev/null
git archive --prefix="sysmon-$version/" -o "packaging/dist/sysmon-$version-source.tar.gz" "v$version"
(cd packaging/dist && sha256sum -- * > SHA256SUMS.tmp && mv SHA256SUMS.tmp SHA256SUMS)
ls -1 packaging/dist/

say "GitHub release"
gh release create "v$version" \
    --title "SysMon $version" \
    --notes-file "$notes_file" \
    --latest \
    packaging/dist/*

printf '\033[32m✓ v%s released\033[0m\n' "$version"
echo "install locally:  sudo apt install ./packaging/dist/sysmon_${version}_amd64.deb"
