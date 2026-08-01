#!/usr/bin/env bash
# Build the Debian package for SysMon (the compiled Rust binary)
# straight from the working tree.
#
#   packaging/build-deb.sh   ->  packaging/dist/sysmon_<version>_<arch>.deb
#
# The v1 python-tree packaging lives in git history (tag v1.0.0,
# last shipped as sysmon_1.0.0_all.deb); this deb upgrades it.
set -euo pipefail

project_directory="$(cd "$(dirname "$0")/.." && pwd)"
packaging_directory="$project_directory/packaging"

# The binary: build fresh, package a stripped copy. Version comes
# from the workspace so the filename can never drift from --version.
(cd "$project_directory" && cargo build --release --quiet -p sysmon-app)
binary="$project_directory/target/release/sysmon"
[ -f "$binary" ] || { echo "no release binary at $binary" >&2; exit 1; }
version="$("$binary" --version | awk '{print $2}')"
architecture="$(dpkg --print-architecture)"

staging_directory="$(mktemp -d)"
trap 'rm -rf "$staging_directory"' EXIT

install -d \
    "$staging_directory/DEBIAN" \
    "$staging_directory/usr/bin" \
    "$staging_directory/usr/share/applications" \
    "$staging_directory/usr/share/icons/hicolor/scalable/apps" \
    "$staging_directory/usr/share/man/man1" \
    "$staging_directory/usr/share/doc/sysmon"

install -m 755 "$binary" "$staging_directory/usr/bin/sysmon"
strip --strip-unneeded "$staging_directory/usr/bin/sysmon"

install -m 644 "$project_directory/assets/sysmon.svg" \
    "$staging_directory/usr/share/icons/hicolor/scalable/apps/sysmon.svg"
install -m 644 "$packaging_directory/sysmon.desktop" \
    "$staging_directory/usr/share/applications/sysmon.desktop"

scdoc < "$packaging_directory/sysmon.1.scd" > "$staging_directory/usr/share/man/man1/sysmon.1"
gzip -9n "$staging_directory/usr/share/man/man1/sysmon.1"

install -m 644 "$project_directory/README.md" "$staging_directory/usr/share/doc/sysmon/"
cat > "$staging_directory/usr/share/doc/sysmon/copyright" <<'COPYRIGHT'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: sysmon
Source: https://github.com/RamenFast/sysmon

Files: *
Copyright: 2026 Ben Miller <2bmillerb@gmail.com>
License: GPL-3+
 This program is free software: you can redistribute it and/or modify
 it under the terms of the GNU General Public License as published by
 the Free Software Foundation, either version 3 of the License, or
 (at your option) any later version.
 .
 This program is distributed in the hope that it will be useful,
 but WITHOUT ANY WARRANTY; without even the implied warranty of
 MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 GNU General Public License for more details.
 .
 On Debian systems, the complete text of the GNU General Public
 License version 3 can be found in "/usr/share/common-licenses/GPL-3".
COPYRIGHT
chmod 644 "$staging_directory/usr/share/doc/sysmon/copyright"

installed_size_kilobytes="$(du -sk "$staging_directory/usr" | cut -f1)"
cat > "$staging_directory/DEBIAN/control" <<CONTROL
Package: sysmon
Version: ${version}
Section: utils
Priority: optional
Architecture: ${architecture}
Installed-Size: ${installed_size_kilobytes}
Depends: libc6 (>= 2.34)
Recommends: nethogs, pciutils, xvfb
Maintainer: Ben Miller <2bmillerb@gmail.com>
Homepage: https://github.com/RamenFast/sysmon
Description: Compact system monitor with an agent-drivable API
 Live cards for AMD GPU, memory, CPU, network, disks, and sensors,
 per-process top lists with real application icons, a full sortable
 process table, pop-out card windows that dock back on drop, and six
 built-in themes.
 .
 The same engine answers a JSON API: sysmon probe/tap/ctl/schema and
 a control socket, so scripts and desktop bars can query any system
 state — networking first, with native no-setup per-process TCP
 attribution (nethogs upgrades it to all protocols and users).
CONTROL

mkdir -p "$packaging_directory/dist"
dpkg-deb --build --root-owner-group \
    "$staging_directory" "$packaging_directory/dist/sysmon_${version}_${architecture}.deb"
