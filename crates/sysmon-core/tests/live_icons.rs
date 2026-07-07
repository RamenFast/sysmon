// SPDX-License-Identifier: GPL-3.0-or-later
//! Live icon resolution against this machine's real desktop entries
//! and icon theme (Blossom → Papirus-Dark → … on Ben's box). Prints
//! the resolution table with --nocapture.

use sysmon_core::apps::AppIndex;

#[test]
fn resolves_icons_for_the_apps_actually_installed_here() {
    let index = AppIndex::load();

    // (comm, exe basename, cmdline) triples the process table will
    // realistically hand us.
    let cases: Vec<(&str, Option<&str>, &str)> = vec![
        ("chrome", Some("chrome"), "/opt/google/chrome/chrome"),
        ("cinnamon", Some("cinnamon"), "cinnamon --replace"),
        ("Xorg", Some("Xorg"), "/usr/lib/xorg/Xorg :0"),
        ("nemo", Some("nemo"), "nemo"),
        ("bash", Some("bash"), "bash"),
        ("steam", Some("steam"), "/usr/lib/steam/steam"),
        ("Discord", Some("Discord"), "/usr/share/discord/Discord"),
        (
            "thorium-browser",
            Some("thorium"),
            "/opt/chromium.org/thorium/thorium-browser",
        ),
        ("kworker/0:1", None, ""),
    ];

    let mut resolved_count = 0;
    for (comm, exe, cmdline) in &cases {
        let path = index.icon_for_process(comm, *exe, cmdline, 32);
        println!("{comm:20} → {}", path.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "(none)".into()));
        if path.is_some() {
            resolved_count += 1;
        }
    }

    // The desktop apps must resolve; kernel workers must not.
    assert!(
        resolved_count >= 5,
        "only {resolved_count} of the known apps resolved an icon"
    );
    assert!(
        index.icon_for_process("kworker/0:1", None, "", 32).is_none(),
        "kernel threads must not get app icons"
    );
}
