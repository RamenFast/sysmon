#!/usr/bin/env bash
# Build a Debian package for SysMon.
#
#   Usage:   packaging/build-deb.sh
#   Output:  dist/sysmon_<version>_all.deb
#
# The package installs:
#   /usr/share/sysmon/             the application code and assets
#   /usr/bin/sysmon                launcher
#   /usr/share/applications/       desktop entry (menu integration)
#   /usr/share/icons/hicolor/      application icon
#   /usr/share/doc/sysmon/         README
#
# Dependencies are declared on Ubuntu/Mint archive packages, so recipients
# can install with:  sudo apt install ./sysmon_<version>_all.deb

set -euo pipefail

project_root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(python3 -c "
import sys
sys.path.insert(0, '$project_root')
from sysmon_app import APPLICATION_VERSION
print(APPLICATION_VERSION)
")"

staging_root="$(mktemp -d)"
trap 'rm -rf "$staging_root"' EXIT
package_dir="$staging_root/sysmon_${version}_all"

# ---- application code and assets -> /usr/share/sysmon -----------------------
install -d "$package_dir/usr/share/sysmon"
cp "$project_root/sysmon.py" "$package_dir/usr/share/sysmon/"
cp -r "$project_root/sysmon_app" "$package_dir/usr/share/sysmon/"
cp -r "$project_root/assets" "$package_dir/usr/share/sysmon/"
find "$package_dir" -type d -name __pycache__ -prune -exec rm -rf {} +

# ---- launcher -> /usr/bin/sysmon ---------------------------------------------
install -d "$package_dir/usr/bin"
cat > "$package_dir/usr/bin/sysmon" <<'LAUNCHER'
#!/bin/sh
exec python3 /usr/share/sysmon/sysmon.py "$@"
LAUNCHER
chmod 755 "$package_dir/usr/bin/sysmon"

# ---- desktop entry and icon --------------------------------------------------
# The repo's desktop file points at the development checkout; the packaged
# one launches the installed binary instead.
install -d "$package_dir/usr/share/applications"
sed 's|^Exec=.*|Exec=sysmon|' "$project_root/sysmon.desktop" \
    > "$package_dir/usr/share/applications/sysmon.desktop"
chmod 644 "$package_dir/usr/share/applications/sysmon.desktop"

install -d "$package_dir/usr/share/icons/hicolor/scalable/apps"
cp "$project_root/assets/sysmon.svg" \
    "$package_dir/usr/share/icons/hicolor/scalable/apps/sysmon.svg"

# ---- documentation -----------------------------------------------------------
install -d "$package_dir/usr/share/doc/sysmon"
cp "$project_root/README.md" "$package_dir/usr/share/doc/sysmon/"

# Debian convention: the copyright file references the system copy of the
# GPL-3 rather than shipping the full text again.
cat > "$package_dir/usr/share/doc/sysmon/copyright" <<'COPYRIGHT'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: sysmon

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
chmod 644 "$package_dir/usr/share/doc/sysmon/copyright"

# ---- package metadata ----------------------------------------------------------
installed_size_kilobytes="$(du -sk "$package_dir/usr" | cut -f1)"
install -d "$package_dir/DEBIAN"
cat > "$package_dir/DEBIAN/control" <<CONTROL
Package: sysmon
Version: ${version}
Section: utils
Priority: optional
Architecture: all
Installed-Size: ${installed_size_kilobytes}
Depends: python3 (>= 3.10), python3-gi, python3-gi-cairo, gir1.2-gtk-3.0, python3-psutil
Recommends: nethogs
Maintainer: Ben Miller <2bmillerb@gmail.com>
Description: Compact GPU, memory, CPU, network, and disk monitor
 A compact, theme-aware GTK 3 system monitor for Cinnamon and other
 GTK desktops. Live graphs for AMD GPU, memory, CPU, network, and
 disks, with per-process top lists, a full sortable process table,
 pop-out section windows, compact mode, and switchable themes
 including Blossom (AMOLED) and Funky Pink.
 .
 Per-process network rates need the optional nethogs package with
 packet-capture permission; everything else works out of the box.
CONTROL

# ---- build -------------------------------------------------------------------
mkdir -p "$project_root/dist"
dpkg-deb --build --root-owner-group \
    "$package_dir" "$project_root/dist/sysmon_${version}_all.deb"
