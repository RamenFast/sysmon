#!/usr/bin/env bash
# Build the RPM for SysMon v2 with cargo-generate-rpm (asset table in
# crates/sysmon-app/Cargo.toml under [package.metadata.generate-rpm]).
#
#   packaging/build-rpm.sh   ->  packaging/dist/sysmon-<version>-1.<arch>.rpm
#
# Built on Mint, verified with `rpm --test` — reports from real
# Fedora/RHEL welcome.
set -euo pipefail

project_directory="$(cd "$(dirname "$0")/.." && pwd)"
packaging_directory="$project_directory/packaging"

(cd "$project_directory" && cargo build --release --quiet -p sysmon-app)
# Refuse to ship an unstripped binary: strip in place, loudly.
strip --strip-unneeded "$project_directory/target/release/sysmon" || {
    echo "strip failed - is a sysmon from target/release still running?" >&2
    exit 1
}

# The manpage is staged pre-gzipped for the asset table.
scdoc < "$packaging_directory/sysmon.1.scd" > "$project_directory/target/sysmon.1"
gzip -9nf "$project_directory/target/sysmon.1"

mkdir -p "$packaging_directory/dist"
(cd "$project_directory" && cargo generate-rpm -p crates/sysmon-app -o "$packaging_directory/dist/")
ls -1 "$packaging_directory/dist/"*.rpm
