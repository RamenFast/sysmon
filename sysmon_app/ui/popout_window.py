"""Pop-out windows: any overview section can live in its own small
always-updating window, so you can keep just the GPU card in a screen corner
while the main window is minimized.

The section *widget* moves between the overview page and this window — it is
never duplicated, so it keeps receiving snapshots from the same sampler.

Two ways home: close the pop-out, or drag it onto the main window and drop
it there — the main window watches pop-out movement and docks the card back
on release (see ``SysMonWindow._on_popout_configure_event``).
"""

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, GLib, Gtk

_DRAG_DOCK_GRACE_MILLISECONDS = 1200


class SectionPopoutWindow(Gtk.Window):
    def __init__(self, section_widget, section_key, display_title, on_closed):
        super().__init__(title=f"{display_title} — SysMon")
        self.section_key = section_key
        self._section_widget = section_widget
        self._on_closed = on_closed

        # State for drag-to-dock, read by the main window's configure handler.
        # The grace period stops the initial window placement from counting
        # as a "drag onto the main window".
        self.last_known_geometry = None
        self.drag_dock_armed = False
        GLib.timeout_add(_DRAG_DOCK_GRACE_MILLISECONDS, self._arm_drag_dock)

        header_bar = Gtk.HeaderBar()
        header_bar.set_show_close_button(True)
        header_bar.set_title(display_title)
        header_bar.set_tooltip_text("Drag this window onto the main window to dock it back")

        self.pin_button = Gtk.ToggleButton()
        self.pin_button.set_image(
            Gtk.Image.new_from_icon_name("view-pin-symbolic", Gtk.IconSize.BUTTON)
        )
        self.pin_button.set_tooltip_text("Keep above other windows")
        self.pin_button.get_style_context().add_class("pin-toggle")
        self.pin_button.connect("toggled", self._on_pin_toggled)
        header_bar.pack_start(self.pin_button)
        self.set_titlebar(header_bar)

        self._content_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self._content_box.set_margin_top(8)
        self._content_box.set_margin_bottom(8)
        self._content_box.set_margin_start(8)
        self._content_box.set_margin_end(8)
        self._content_box.pack_start(section_widget, False, False, 0)
        self.add(self._content_box)

        self.set_default_size(370, -1)
        self.connect("delete-event", self._on_delete_event)

        # The whole body is a drag handle, not just the slim header bar —
        # pop-outs are meant to be tossed into a screen corner. Presses that
        # a child widget consumes (the pin toggle, a top-process right-click)
        # never reach this handler.
        self.add_events(Gdk.EventMask.BUTTON_PRESS_MASK)
        self.connect("button-press-event", self._on_body_button_press)

        self.show_all()

        # Pop-outs exist to stay visible while you work — pinned by default.
        self.pin_button.set_active(True)

    def _arm_drag_dock(self):
        self.drag_dock_armed = True
        return False  # one-shot

    def _on_body_button_press(self, _window, event):
        if event.button == 1 and event.type == Gdk.EventType.BUTTON_PRESS:
            self.begin_move_drag(
                event.button, int(event.x_root), int(event.y_root), event.time
            )
            return True
        return False

    def _on_pin_toggled(self, button):
        self.set_keep_above(button.get_active())

    def release_section(self):
        """Detach the section widget so it can be reattached elsewhere."""
        self._content_box.remove(self._section_widget)
        return self._section_widget

    def _on_delete_event(self, _window, _event):
        self._on_closed(self)
        return False  # continue with destruction
