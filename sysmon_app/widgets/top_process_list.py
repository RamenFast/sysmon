"""The compact 'top 3 processes' list shown inside each overview section.

Rows show rank, process name, and a formatted value. When process actions are
enabled, right-clicking a row opens the shared end/kill/priority menu.
"""

from dataclasses import dataclass

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, Gtk, Pango

from .process_actions import build_process_context_menu


@dataclass
class TopProcessEntry:
    pid: int
    display_name: str
    formatted_value: str
    nice_value: int = None


class TopProcessList(Gtk.Box):
    def __init__(self, row_count=3, enable_process_actions=True):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=2)
        self._enable_process_actions = enable_process_actions
        self._rows = [self._build_row(rank) for rank in range(1, row_count + 1)]
        for row in self._rows:
            self.pack_start(row["event_box"], False, False, 0)

    def _build_row(self, rank):
        row_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)

        rank_label = Gtk.Label(label=f"{rank}")
        rank_label.get_style_context().add_class("top-process-rank")
        rank_label.set_width_chars(2)
        rank_label.set_xalign(0.0)
        row_box.pack_start(rank_label, False, False, 0)

        name_label = Gtk.Label(label="—")
        name_label.set_ellipsize(Pango.EllipsizeMode.END)
        name_label.set_xalign(0.0)
        row_box.pack_start(name_label, True, True, 0)

        value_label = Gtk.Label(label="")
        value_label.get_style_context().add_class("tabular-number")
        value_label.set_xalign(1.0)
        row_box.pack_end(value_label, False, False, 0)

        event_box = Gtk.EventBox()
        event_box.add(row_box)
        event_box.add_events(Gdk.EventMask.BUTTON_PRESS_MASK)
        row = {"event_box": event_box, "name_label": name_label,
               "value_label": value_label, "entry": None}
        if self._enable_process_actions:
            event_box.connect("button-press-event", self._on_row_button_press, row)
        return row

    def _on_row_button_press(self, _event_box, event, row):
        entry = row["entry"]
        if event.button != 3 or entry is None:
            return False
        parent_window = self.get_toplevel()
        menu = build_process_context_menu(
            entry.pid, entry.display_name, parent_window, entry.nice_value
        )
        menu.popup_at_pointer(event)
        return True

    def update_entries(self, entries):
        for row_index, row in enumerate(self._rows):
            if row_index < len(entries):
                entry = entries[row_index]
                row["entry"] = entry
                row["name_label"].set_label(entry.display_name)
                row["name_label"].set_tooltip_text(f"PID {entry.pid}")
                row["value_label"].set_label(entry.formatted_value)
            else:
                row["entry"] = None
                row["name_label"].set_label("—")
                row["name_label"].set_tooltip_text(None)
                row["value_label"].set_label("")
