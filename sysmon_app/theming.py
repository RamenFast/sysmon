"""Theme handling: dark/light switching that respects the Cinnamon GTK theme,
plus the theme-aware colour palette used by the graphs.

Switching strategy: Mint ships dark variants as separately named themes
(e.g. Mint-Y-Aqua vs Mint-Y-Dark-Aqua), so flipping the usual
``gtk-application-prefer-dark-theme`` flag alone is not enough.  When asked
for dark or light we look for the matching variant of the *current* theme on
disk and swap the theme name for this process only — the rest of the desktop
is untouched.
"""

import os

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, Gtk

from .settings import (
    THEME_MODE_BLOSSOM,
    THEME_MODE_DARK,
    THEME_MODE_FOLLOW_SYSTEM,
    THEME_MODE_FUNKY,
    THEME_MODE_LIGHT,
)

_THEME_SEARCH_DIRECTORIES = [
    os.path.expanduser("~/.themes"),
    os.path.expanduser("~/.local/share/themes"),
    "/usr/share/themes",
]

# Remembered at import time so "follow system" can restore the user's theme.
_system_theme_name = None
_system_prefer_dark = None


def remember_system_theme():
    global _system_theme_name, _system_prefer_dark
    gtk_settings = Gtk.Settings.get_default()
    _system_theme_name = gtk_settings.get_property("gtk-theme-name")
    _system_prefer_dark = gtk_settings.get_property("gtk-application-prefer-dark-theme")


def _theme_exists_on_disk(theme_name):
    for directory in _THEME_SEARCH_DIRECTORIES:
        if os.path.isdir(os.path.join(directory, theme_name, "gtk-3.0")):
            return True
    return False


def _dark_variant_of(theme_name):
    """Best-effort dark variant name for a theme, or None."""
    if "dark" in theme_name.lower():
        return theme_name
    name_parts = theme_name.split("-")
    # Try inserting "Dark" at every position after the base name:
    # Mint-Y-Aqua -> Mint-Y-Dark-Aqua, Mint-Y -> Mint-Y-Dark, Adwaita -> Adwaita-dark
    candidates = []
    for insert_position in range(len(name_parts), 0, -1):
        candidates.append("-".join(name_parts[:insert_position] + ["Dark"] + name_parts[insert_position:]))
    candidates.append(theme_name + "-dark")
    for candidate in candidates:
        if _theme_exists_on_disk(candidate):
            return candidate
    return None


def _light_variant_of(theme_name):
    """Best-effort light variant name for a theme, or None."""
    lowered_parts = [part.lower() for part in theme_name.split("-")]
    if "dark" not in lowered_parts and "darker" not in lowered_parts:
        return theme_name
    kept_parts = [
        part for part in theme_name.split("-") if part.lower() not in ("dark", "darker")
    ]
    candidate = "-".join(kept_parts)
    if candidate and _theme_exists_on_disk(candidate):
        return candidate
    return None


def apply_theme_mode(theme_mode):
    """Apply the requested theme mode to this process's Gtk.Settings."""
    gtk_settings = Gtk.Settings.get_default()
    if _system_theme_name is None:
        remember_system_theme()

    _set_theme_overlay_css(theme_mode)

    if theme_mode == THEME_MODE_FOLLOW_SYSTEM:
        gtk_settings.set_property("gtk-theme-name", _system_theme_name)
        gtk_settings.set_property("gtk-application-prefer-dark-theme", _system_prefer_dark)
        return

    if theme_mode in (THEME_MODE_DARK, THEME_MODE_BLOSSOM):
        # Blossom builds on the dark theme variant, then paints over it with
        # AMOLED-black CSS.
        variant = _dark_variant_of(_system_theme_name)
        if variant:
            gtk_settings.set_property("gtk-theme-name", variant)
        gtk_settings.set_property("gtk-application-prefer-dark-theme", True)
        return

    if theme_mode in (THEME_MODE_LIGHT, THEME_MODE_FUNKY):
        # Funky builds on the light theme variant, then paints over it with
        # bubblegum-pink CSS.
        variant = _light_variant_of(_system_theme_name)
        if variant:
            gtk_settings.set_property("gtk-theme-name", variant)
        gtk_settings.set_property("gtk-application-prefer-dark-theme", False)


def widget_uses_dark_theme(widget):
    """Detect dark themes by the luminance of the widget's background."""
    style_context = widget.get_style_context()
    found, background = style_context.lookup_color("theme_bg_color")
    if not found:
        return False
    luminance = 0.2126 * background.red + 0.7152 * background.green + 0.0722 * background.blue
    return luminance < 0.5


# Graph colour palettes. Every palette defines all five section colours for
# both light and dark themes, so switching is always safe.
GRAPH_PALETTES = {
    "mint": {
        "display_name": "Mint",
        "light": {
            "gpu": (0.80, 0.25, 0.20),
            "memory": (0.47, 0.32, 0.74),
            "cpu": (0.14, 0.45, 0.80),
            "network_download": (0.13, 0.55, 0.30),
            "network_upload": (0.82, 0.48, 0.08),
        },
        "dark": {
            "gpu": (0.96, 0.48, 0.40),
            "memory": (0.72, 0.58, 0.95),
            "cpu": (0.42, 0.69, 0.97),
            "network_download": (0.44, 0.83, 0.56),
            "network_upload": (0.96, 0.68, 0.32),
        },
    },
    "aqua": {
        "display_name": "Aqua",
        "light": {
            "gpu": (0.00, 0.50, 0.66),
            "memory": (0.27, 0.36, 0.74),
            "cpu": (0.00, 0.56, 0.53),
            "network_download": (0.07, 0.48, 0.78),
            "network_upload": (0.52, 0.38, 0.72),
        },
        "dark": {
            "gpu": (0.22, 0.76, 0.92),
            "memory": (0.56, 0.64, 0.98),
            "cpu": (0.28, 0.85, 0.79),
            "network_download": (0.38, 0.72, 0.98),
            "network_upload": (0.78, 0.63, 0.96),
        },
    },
    "sunset": {
        "display_name": "Sunset",
        "light": {
            "gpu": (0.80, 0.20, 0.26),
            "memory": (0.71, 0.25, 0.46),
            "cpu": (0.84, 0.44, 0.10),
            "network_download": (0.72, 0.54, 0.05),
            "network_upload": (0.55, 0.27, 0.52),
        },
        "dark": {
            "gpu": (0.98, 0.46, 0.43),
            "memory": (0.94, 0.52, 0.66),
            "cpu": (0.98, 0.66, 0.32),
            "network_download": (0.96, 0.79, 0.36),
            "network_upload": (0.82, 0.57, 0.86),
        },
    },
    "forest": {
        "display_name": "Forest",
        "light": {
            "gpu": (0.21, 0.52, 0.26),
            "memory": (0.10, 0.45, 0.40),
            "cpu": (0.44, 0.52, 0.14),
            "network_download": (0.14, 0.49, 0.55),
            "network_upload": (0.60, 0.44, 0.18),
        },
        "dark": {
            "gpu": (0.47, 0.82, 0.52),
            "memory": (0.42, 0.77, 0.66),
            "cpu": (0.72, 0.81, 0.42),
            "network_download": (0.46, 0.79, 0.84),
            "network_upload": (0.87, 0.72, 0.46),
        },
    },
    "mono": {
        "display_name": "Mono",
        "light": {
            "gpu": (0.29, 0.32, 0.37),
            "memory": (0.29, 0.32, 0.37),
            "cpu": (0.29, 0.32, 0.37),
            "network_download": (0.29, 0.32, 0.37),
            "network_upload": (0.56, 0.59, 0.64),
        },
        "dark": {
            "gpu": (0.78, 0.80, 0.84),
            "memory": (0.78, 0.80, 0.84),
            "cpu": (0.78, 0.80, 0.84),
            "network_download": (0.78, 0.80, 0.84),
            "network_upload": (0.53, 0.55, 0.60),
        },
    },
    "blossom": {
        # Soft pinks and golds, made to glow on the AMOLED-black appearance.
        "display_name": "Blossom",
        "light": {
            "gpu": (0.84, 0.41, 0.55),
            "memory": (0.72, 0.55, 0.17),
            "cpu": (0.77, 0.32, 0.47),
            "network_download": (0.69, 0.54, 0.14),
            "network_upload": (0.62, 0.40, 0.53),
        },
        "dark": {
            "gpu": (0.97, 0.66, 0.75),
            "memory": (0.92, 0.77, 0.43),
            "cpu": (0.94, 0.52, 0.65),
            "network_download": (0.96, 0.85, 0.59),
            "network_upload": (0.81, 0.56, 0.67),
        },
    },
    "funky": {
        # Loud, clashing brights for the bubblegum Funky Pink appearance.
        "display_name": "Funky",
        "light": {
            "gpu": (0.93, 0.13, 0.57),
            "memory": (0.55, 0.17, 0.89),
            "cpu": (0.00, 0.65, 0.62),
            "network_download": (0.10, 0.50, 0.95),
            "network_upload": (0.98, 0.55, 0.00),
        },
        "dark": {
            "gpu": (1.00, 0.42, 0.71),
            "memory": (0.78, 0.55, 1.00),
            "cpu": (0.25, 0.90, 0.85),
            "network_download": (0.40, 0.75, 1.00),
            "network_upload": (1.00, 0.72, 0.30),
        },
    },
}

_active_graph_palette_name = "mint"


def set_graph_palette(palette_name):
    global _active_graph_palette_name
    if palette_name in GRAPH_PALETTES:
        _active_graph_palette_name = palette_name


def available_graph_palettes():
    """[(palette_key, display_name), ...] in menu order."""
    return [(key, value["display_name"]) for key, value in GRAPH_PALETTES.items()]


class GraphPalette:
    """Per-section graph colours from the active palette, tuned per theme."""

    @staticmethod
    def color(color_key, is_dark_theme):
        palette = GRAPH_PALETTES[_active_graph_palette_name]
        return palette["dark" if is_dark_theme else "light"][color_key]


APPLICATION_CSS = b"""
.metric-card {
    background-color: alpha(@theme_fg_color, 0.045);
    border: 1px solid alpha(@theme_fg_color, 0.09);
    border-radius: 10px;
    padding: 8px;
}
.section-title {
    font-weight: 600;
    font-size: 92%;
}
.headline-value {
    font-weight: 600;
    font-size: 92%;
    font-feature-settings: "tnum";
}
.tabular-number {
    font-feature-settings: "tnum";
}
.dim-small-label {
    opacity: 0.6;
    font-size: 85%;
}
.stat-value-label {
    font-feature-settings: "tnum";
    font-size: 92%;
}
.top-process-rank {
    opacity: 0.45;
    font-size: 85%;
}
.unavailable-hint {
    opacity: 0.55;
    font-size: 82%;
    font-style: italic;
}

/* The small pop-out toggle in every section header: visible as a button at
   rest, accent-tinted while its section is popped out. */
.pop-out-toggle {
    min-height: 0;
    min-width: 0;
    padding: 1px 5px;
    border: 1px solid alpha(@theme_fg_color, 0.22);
    border-radius: 6px;
    background-color: alpha(@theme_fg_color, 0.05);
    background-image: none;
    box-shadow: none;
    opacity: 0.75;
}
.pop-out-toggle:hover {
    opacity: 1.0;
    background-color: alpha(@theme_fg_color, 0.12);
}
.pop-out-toggle:checked {
    opacity: 1.0;
    color: @theme_selected_bg_color;
    border-color: alpha(@theme_selected_bg_color, 0.7);
    background-color: alpha(@theme_selected_bg_color, 0.18);
}

/* Pin toggles (header bar + pop-out windows): unmistakable when engaged. */
.pin-toggle:checked {
    color: @theme_selected_bg_color;
    background-color: alpha(@theme_selected_bg_color, 0.22);
    background-image: none;
}
"""

# Painted on top of the dark theme when the Blossom appearance is active:
# true-black surfaces for AMOLED, soft pink titles, gold headline numbers.
BLOSSOM_CSS = b"""
window {
    background-color: #000000;
}
stack, scrolledwindow, viewport {
    background-color: #000000;
}
headerbar {
    background-image: none;
    background-color: #070506;
    border-bottom-color: alpha(#f3b2c4, 0.15);
}
popover, popover.background {
    background-color: #0c0809;
}
.metric-card {
    background-color: #0c0809;
    border-color: alpha(#f3b2c4, 0.16);
}
.section-title {
    color: #f3b2c4;
}
.headline-value {
    color: #e8c87e;
}
treeview.view {
    background-color: #050304;
}
"""

# Painted on top of the light theme when the Funky Pink appearance is active:
# bubblegum backgrounds, a hot-pink-to-violet header, wonky asymmetric card
# corners, and chunky pop-art offset shadows.
FUNKY_CSS = b"""
window {
    background-color: #ffddee;
}
stack, scrolledwindow, viewport {
    background-color: #ffddee;
}
headerbar {
    background-image: linear-gradient(135deg, #ff4fa0, #c95bff);
    background-color: #ff4fa0;
    border-bottom: 2px solid #e0218a;
    color: #ffffff;
}
headerbar label {
    color: #ffffff;
    text-shadow: 1px 1px 0 alpha(#8a1257, 0.45);
}
headerbar button {
    border-radius: 999px;
}
popover, popover.background {
    background-color: #fff5fa;
    border: 2px solid #ff8fc5;
    border-radius: 16px;
}
.metric-card {
    background-color: #fff5fa;
    border: 2px solid #ff8fc5;
    border-radius: 24px 8px 24px 8px;
    padding: 10px;
    box-shadow: 5px 5px 0 alpha(#ff2d92, 0.28);
}
.section-title {
    color: #e0218a;
    font-size: 100%;
}
.headline-value {
    color: #7a1fd0;
}
.dim-small-label {
    color: #a84a85;
    opacity: 0.9;
}
.pop-out-toggle {
    border: 2px solid #ff8fc5;
    border-radius: 999px;
    background-color: alpha(#ff8fc5, 0.18);
}
.pop-out-toggle:checked {
    color: #e0218a;
    border-color: #e0218a;
    background-color: alpha(#e0218a, 0.20);
}
.pin-toggle:checked {
    color: #e0218a;
    background-color: alpha(#e0218a, 0.20);
}
levelbar block.filled {
    background-color: #ff4fa0;
    border-color: #e0218a;
}
treeview.view {
    background-color: #fff5fa;
}
treeview.view:selected {
    background-color: #ff4fa0;
}
scrollbar slider {
    background-color: #ff8fc5;
    border-radius: 999px;
}
"""

# Each special appearance paints its CSS overlay on top of the base theme;
# plain light/dark/system modes run with no overlay at all.
_THEME_OVERLAY_CSS = {
    THEME_MODE_BLOSSOM: BLOSSOM_CSS,
    THEME_MODE_FUNKY: FUNKY_CSS,
}

_active_overlay_provider = None
_active_overlay_theme_mode = None


def _set_theme_overlay_css(theme_mode):
    global _active_overlay_provider, _active_overlay_theme_mode
    if theme_mode == _active_overlay_theme_mode:
        return
    screen = Gdk.Screen.get_default()
    if _active_overlay_provider is not None:
        Gtk.StyleContext.remove_provider_for_screen(screen, _active_overlay_provider)
        _active_overlay_provider = None
    overlay_css = _THEME_OVERLAY_CSS.get(theme_mode)
    if overlay_css is not None:
        provider = Gtk.CssProvider()
        provider.load_from_data(overlay_css)
        Gtk.StyleContext.add_provider_for_screen(
            screen, provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION + 1
        )
        _active_overlay_provider = provider
    _active_overlay_theme_mode = theme_mode


def install_application_css():
    css_provider = Gtk.CssProvider()
    css_provider.load_from_data(APPLICATION_CSS)
    Gtk.StyleContext.add_provider_for_screen(
        Gdk.Screen.get_default(),
        css_provider,
        Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION,
    )
