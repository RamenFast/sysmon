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
grep -q "sysmon_${version}_amd64.deb" README.md || \
    fail "README install commands don't mention $version — update them (the page must never show a stale version)"

say "version string is whole"
# release.sh once checked only the numeric field, so `sysmon 3.0.0 (v2)`
# would have shipped with a stale era suffix. The whole line is the claim.
expected_version_line="sysmon $version (v${version%%.*})"
actual_version_line="$(target/release/sysmon --version)"
[ "$actual_version_line" = "$expected_version_line" ] || \
    fail "--version says '$actual_version_line', expected '$expected_version_line'"

say "README carries every install command for $version"
for expected in "sysmon_${version}_amd64.deb" "sysmon-${version}-1.x86_64.rpm" "$expected_version_line"; do
    grep -q -- "$expected" README.md || \
        fail "README does not mention '$expected' — the page must never show a stale version"
done

say "test suite"
cargo test --release --quiet 2>&1 | tail -2

say "conformance to the workspace CLI standard"
# The standard is a gate, not a wish: a release that fails a clause of
# AGENT-CLI-STANDARD.md does not leave this machine.
scripts/conformance.sh target/release/sysmon >/dev/null || \
    fail "conformance failed — run scripts/conformance.sh to see which clause"

say "end-to-end receipts on a private display"
scripts/e2e.sh "$(mktemp -d)/e2e-out" >/dev/null || fail "e2e.sh failed"

say "build the artifacts BEFORE anything is published"
# Packaging used to run after the tag was pushed, so a packaging
# failure left a published tag with no assets behind it. Build first;
# publish only what already exists.
rm -f packaging/dist/*
packaging/build-deb.sh >/dev/null
packaging/build-rpm.sh >/dev/null

say "tag v$version + push"
git tag -a "v$version" -m "SysMon $version"
git push -q origin main "v$version"

say "source tarball + checksums"
git archive --prefix="sysmon-$version/" -o "packaging/dist/sysmon-$version-source.tar.gz" "v$version"
(cd packaging/dist && sha256sum -- * > SHA256SUMS.tmp && mv SHA256SUMS.tmp SHA256SUMS)
(cd packaging/dist && sha256sum -c SHA256SUMS >/dev/null) || fail "checksums do not verify"
ls -1 packaging/dist/

say "GitHub release"
gh release create "v$version" \
    --title "SysMon $version" \
    --notes-file "$notes_file" \
    --latest \
    packaging/dist/*

printf '\033[32m✓ v%s released\033[0m\n' "$version"
echo
echo "install it (the release is not done until it runs on this machine):"
echo "  sudoplz sudo apt install -y ./packaging/dist/sysmon_${version}_amd64.deb"
echo "then confirm:  sysmon --version   →  $expected_version_line"
