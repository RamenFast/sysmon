"""Persisted user preferences, stored at ~/.config/sysmon/settings.json."""

import json
import os
from dataclasses import dataclass, field, asdict

SETTINGS_DIRECTORY = os.path.join(
    os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config")), "sysmon"
)
SETTINGS_FILE_PATH = os.path.join(SETTINGS_DIRECTORY, "settings.json")

THEME_MODE_FOLLOW_SYSTEM = "system"
THEME_MODE_LIGHT = "light"
THEME_MODE_DARK = "dark"
THEME_MODE_BLOSSOM = "blossom"  # AMOLED black with pink/gold accents
THEME_MODE_FUNKY = "funky"  # bubblegum-pink, wonky corners, pop-art shadows

SECTION_KEYS = ["gpu", "memory", "cpu", "network", "disks"]


def _default_visible_sections():
    return {key: True for key in SECTION_KEYS}


@dataclass
class UserSettings:
    theme_mode: str = THEME_MODE_FOLLOW_SYSTEM
    update_interval_seconds: float = 2.0
    always_on_top: bool = False
    compact_mode: bool = False
    show_pin_button: bool = True
    graph_palette: str = "mint"
    use_binary_units: bool = False  # GiB/MiB (htop-style) instead of GB/MB
    visible_sections: dict = field(default_factory=_default_visible_sections)
    popped_out_sections: list = field(default_factory=list)
    window_width: int = 390
    window_height: int = 780

    @classmethod
    def load(cls):
        try:
            with open(SETTINGS_FILE_PATH, "r", encoding="utf-8") as settings_file:
                stored = json.load(settings_file)
        except (OSError, ValueError):
            return cls()

        settings = cls()
        for field_name in (
            "theme_mode",
            "update_interval_seconds",
            "always_on_top",
            "compact_mode",
            "show_pin_button",
            "graph_palette",
            "use_binary_units",
            "window_width",
            "window_height",
        ):
            if field_name in stored:
                setattr(settings, field_name, stored[field_name])
        stored_sections = stored.get("visible_sections", {})
        for section_key in SECTION_KEYS:
            if section_key in stored_sections:
                settings.visible_sections[section_key] = bool(stored_sections[section_key])
        settings.popped_out_sections = [
            section_key
            for section_key in stored.get("popped_out_sections", [])
            if section_key in SECTION_KEYS
        ]
        return settings

    def save(self):
        try:
            os.makedirs(SETTINGS_DIRECTORY, exist_ok=True)
            with open(SETTINGS_FILE_PATH, "w", encoding="utf-8") as settings_file:
                json.dump(asdict(self), settings_file, indent=2)
        except OSError:
            pass  # Preferences are a convenience; never crash over them.
