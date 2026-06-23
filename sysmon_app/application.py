"""Application shell: single-instance Gtk.Application that owns the window."""

import os
import sys

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, GLib, Gtk

from .theming import install_application_css, remember_system_theme
from .ui.window import SysMonWindow

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APPLICATION_ICON_PATH = os.path.join(PROJECT_ROOT, "assets", "sysmon.svg")

_ICON_SIZES = [16, 24, 32, 48, 64, 128, 256]


def _install_default_window_icons():
    """Render the SVG at every common size so panels, switchers, and window
    decorations all get a crisp icon."""
    icon_pixbufs = []
    for size in _ICON_SIZES:
        try:
            icon_pixbufs.append(
                GdkPixbuf.Pixbuf.new_from_file_at_size(APPLICATION_ICON_PATH, size, size)
            )
        except GLib.Error as error:
            print(f"sysmon: could not render icon at {size}px: {error}", file=sys.stderr)
    if icon_pixbufs:
        Gtk.Window.set_default_icon_list(icon_pixbufs)


class SysMonApplication(Gtk.Application):
    def __init__(self):
        super().__init__(application_id="dev.claudeworkspace.SysMon")
        self._window = None

    def do_startup(self):
        Gtk.Application.do_startup(self)
        GLib.set_application_name("SysMon")
        _install_default_window_icons()
        remember_system_theme()
        install_application_css()

    def do_activate(self):
        if self._window is None:
            self._window = SysMonWindow(self)
        self._window.present()
