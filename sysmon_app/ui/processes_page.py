"""The Processes page: a full, sortable, searchable process table.

Raw numeric values live in the ListStore so column sorting is numeric; the
visible text is produced by cell data functions. Rows are updated in place
(keyed by PID) so selection and scroll position survive refreshes.
"""

import os

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import Gdk, GdkPixbuf, Gio, GLib, GObject, Gtk, Pango

from ..formatting import format_rate, format_size
from ..widgets.process_actions import build_process_context_menu

_PROCESS_ICON_SIZE = 16
_LINUX_COMM_NAME_LIMIT = 15  # /proc comm truncates names to 15 characters


class ProcessIconResolver:
    """Best-effort mapping from a process name to a small themed icon.

    Strategy: try the icon theme directly (works for firefox, discord, …),
    then fall back to a table built from installed .desktop files keyed by
    executable name. Everything is cached; unknown processes get a generic
    executable icon.
    """

    def __init__(self, application_icon_path=None):
        self._icon_theme = Gtk.IconTheme.get_default()
        self._pixbuf_cache = {}
        self._fallback_pixbuf = self._load_pixbuf("application-x-executable")
        self._icon_name_by_executable = self._index_installed_applications()
        if application_icon_path is not None:
            self._pixbuf_cache["sysmon"] = (
                self._load_pixbuf(application_icon_path) or self._fallback_pixbuf
            )

    @staticmethod
    def _index_installed_applications():
        icon_names = {}
        try:
            installed_applications = Gio.AppInfo.get_all()
        except GLib.Error:
            return icon_names
        for app_info in installed_applications:
            icon = app_info.get_icon()
            if icon is None:
                continue
            icon_name = icon.to_string()
            executable = os.path.basename(app_info.get_executable() or "")
            desktop_id = (app_info.get_id() or "").removesuffix(".desktop")
            for key in {
                executable.lower(),
                executable.lower()[:_LINUX_COMM_NAME_LIMIT],
                desktop_id.lower(),
            }:
                if key:
                    icon_names.setdefault(key, icon_name)
        return icon_names

    def _load_pixbuf(self, icon_name_or_path):
        try:
            if icon_name_or_path.startswith("/"):
                return GdkPixbuf.Pixbuf.new_from_file_at_size(
                    icon_name_or_path, _PROCESS_ICON_SIZE, _PROCESS_ICON_SIZE
                )
            return self._icon_theme.load_icon(
                icon_name_or_path, _PROCESS_ICON_SIZE, Gtk.IconLookupFlags.FORCE_SIZE
            )
        except GLib.Error:
            return None

    def pixbuf_for_process_name(self, process_name):
        cache_key = process_name.lower()
        cached = self._pixbuf_cache.get(cache_key)
        if cached is not None:
            return cached

        if self._icon_theme.has_icon(cache_key):
            icon_name = cache_key
        else:
            icon_name = self._icon_name_by_executable.get(cache_key)
        pixbuf = self._load_pixbuf(icon_name) if icon_name else None
        if pixbuf is None:
            pixbuf = self._fallback_pixbuf
        self._pixbuf_cache[cache_key] = pixbuf
        return pixbuf

# Model column indexes.
COLUMN_PID = 0
COLUMN_NAME = 1
COLUMN_USER = 2
COLUMN_CPU_PERCENT = 3
COLUMN_MEMORY_BYTES = 4
COLUMN_GPU_PERCENT = 5
COLUMN_VRAM_BYTES = 6
COLUMN_READ_RATE = 7
COLUMN_WRITE_RATE = 8
COLUMN_NETWORK_RATE = 9  # -1 means "no data source"
COLUMN_NICE = 10
COLUMN_COMMAND = 11
COLUMN_ICON = 12

_MODEL_COLUMN_TYPES = [
    int,                  # pid
    str,                  # name
    str,                  # user
    float,                # cpu %
    GObject.TYPE_UINT64,  # memory bytes
    float,                # gpu %
    GObject.TYPE_UINT64,  # vram bytes
    float,                # disk read B/s
    float,                # disk write B/s
    float,                # network B/s
    int,                  # nice
    str,                  # command line
    GdkPixbuf.Pixbuf,     # process icon
]


def _format_optional_rate(rate_value):
    if rate_value < 0:
        return "—"
    if rate_value < 1024:  # hide sub-KiB noise so the column stays readable
        return ""
    return format_rate(rate_value)


def _format_optional_percent(percent_value):
    return f"{percent_value:.1f}" if percent_value >= 0.05 else ""


class ProcessesPage(Gtk.Box):
    def __init__(self):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=0)
        self._iter_by_pid = {}
        self._search_text = ""
        self._latest_bundle = None

        from ..application import APPLICATION_ICON_PATH

        self._icon_resolver = ProcessIconResolver(APPLICATION_ICON_PATH)
        self.pack_start(self._build_toolbar(), False, False, 0)
        self.pack_start(self._build_table(), True, True, 0)

    # ----------------------------------------------------------------- build

    def _build_toolbar(self):
        toolbar = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        toolbar.set_margin_top(8)
        toolbar.set_margin_bottom(8)
        toolbar.set_margin_start(10)
        toolbar.set_margin_end(10)

        self.search_entry = Gtk.SearchEntry()
        self.search_entry.set_placeholder_text("Filter by name, command, or PID…")
        self.search_entry.connect("search-changed", self._on_search_changed)
        toolbar.pack_start(self.search_entry, True, True, 0)

        self.process_count_label = Gtk.Label(label="")
        self.process_count_label.get_style_context().add_class("dim-small-label")
        toolbar.pack_end(self.process_count_label, False, False, 0)
        return toolbar

    def _build_table(self):
        self.model = Gtk.ListStore(*_MODEL_COLUMN_TYPES)
        self.model.set_sort_column_id(COLUMN_CPU_PERCENT, Gtk.SortType.DESCENDING)

        self.tree_view = Gtk.TreeView(model=self.model)
        self.tree_view.set_fixed_height_mode(True)
        self.tree_view.connect("button-press-event", self._on_button_press)
        self.tree_view.connect("popup-menu", self._on_popup_menu)

        column_definitions = [
            ("Name", COLUMN_NAME, self._render_text(COLUMN_NAME), 160, True),
            ("PID", COLUMN_PID, self._render_number(COLUMN_PID, "{}"), 64, False),
            ("User", COLUMN_USER, self._render_text(COLUMN_USER), 80, False),
            ("CPU %", COLUMN_CPU_PERCENT,
             self._render_number(COLUMN_CPU_PERCENT, "{:.1f}"), 70, False),
            ("Memory", COLUMN_MEMORY_BYTES,
             self._render_formatted(COLUMN_MEMORY_BYTES, format_size), 88, False),
            ("GPU %", COLUMN_GPU_PERCENT,
             self._render_formatted(COLUMN_GPU_PERCENT, _format_optional_percent), 66, False),
            ("VRAM", COLUMN_VRAM_BYTES,
             self._render_formatted(COLUMN_VRAM_BYTES,
                                    lambda v: format_size(v) if v else ""), 80, False),
            ("Read/s", COLUMN_READ_RATE,
             self._render_formatted(COLUMN_READ_RATE, _format_optional_rate), 86, False),
            ("Write/s", COLUMN_WRITE_RATE,
             self._render_formatted(COLUMN_WRITE_RATE, _format_optional_rate), 86, False),
            ("Net/s", COLUMN_NETWORK_RATE,
             self._render_formatted(COLUMN_NETWORK_RATE, _format_optional_rate), 86, False),
            ("Pri", COLUMN_NICE, self._render_number(COLUMN_NICE, "{}"), 50, False),
            ("Command", COLUMN_COMMAND, self._render_text(COLUMN_COMMAND), 200, True),
        ]

        for title, sort_column, (renderer, data_function), width, expand in column_definitions:
            column = Gtk.TreeViewColumn(title)
            if sort_column == COLUMN_NAME:
                icon_renderer = Gtk.CellRendererPixbuf()
                column.pack_start(icon_renderer, False)
                column.add_attribute(icon_renderer, "pixbuf", COLUMN_ICON)
            column.pack_start(renderer, True)
            column.set_cell_data_func(renderer, data_function)
            column.set_sort_column_id(sort_column)
            column.set_resizable(True)
            column.set_sizing(Gtk.TreeViewColumnSizing.FIXED)
            column.set_fixed_width(width)
            column.set_expand(expand)
            self.tree_view.append_column(column)

        scrolled_window = Gtk.ScrolledWindow()
        scrolled_window.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
        scrolled_window.add(self.tree_view)
        return scrolled_window

    def _render_text(self, model_column):
        renderer = Gtk.CellRendererText()
        renderer.set_property("ellipsize", Pango.EllipsizeMode.END)

        def data_function(_column, cell, model, tree_iter, _data):
            cell.set_property("text", model[tree_iter][model_column])

        return renderer, data_function

    def _render_number(self, model_column, format_string):
        renderer = Gtk.CellRendererText()
        renderer.set_property("xalign", 1.0)

        def data_function(_column, cell, model, tree_iter, _data):
            cell.set_property("text", format_string.format(model[tree_iter][model_column]))

        return renderer, data_function

    def _render_formatted(self, model_column, formatter):
        renderer = Gtk.CellRendererText()
        renderer.set_property("xalign", 1.0)

        def data_function(_column, cell, model, tree_iter, _data):
            cell.set_property("text", formatter(model[tree_iter][model_column]))

        return renderer, data_function

    # ---------------------------------------------------------------- update

    def _on_search_changed(self, search_entry):
        self._search_text = search_entry.get_text().lower().strip()
        if self._latest_bundle is not None:
            self.update(self._latest_bundle)

    def _record_matches_search(self, record):
        if not self._search_text:
            return True
        return (
            self._search_text in record.name.lower()
            or self._search_text in record.command_line.lower()
            or self._search_text == str(record.pid)
        )

    def update(self, bundle):
        self._latest_bundle = bundle
        visible_records = {
            pid: record
            for pid, record in bundle.processes.items()
            if self._record_matches_search(record)
        }

        update_columns = [
            COLUMN_NAME, COLUMN_USER, COLUMN_CPU_PERCENT, COLUMN_MEMORY_BYTES,
            COLUMN_GPU_PERCENT, COLUMN_VRAM_BYTES, COLUMN_READ_RATE,
            COLUMN_WRITE_RATE, COLUMN_NETWORK_RATE, COLUMN_NICE, COLUMN_COMMAND,
        ]

        for pid, record in visible_records.items():
            values = [
                record.name,
                record.username,
                record.cpu_percent,
                record.resident_memory_bytes,
                record.gpu_busy_percent,
                record.gpu_vram_bytes,
                record.disk_read_bytes_per_second,
                record.disk_write_bytes_per_second,
                record.network_bytes_per_second
                if record.network_bytes_per_second is not None
                else -1.0,
                record.nice_value,
                record.command_line,
            ]
            existing_iter = self._iter_by_pid.get(pid)
            if existing_iter is not None:
                self.model.set(existing_iter, update_columns, values)
            else:
                # The icon is resolved once per process, on insertion only.
                icon_pixbuf = self._icon_resolver.pixbuf_for_process_name(record.name)
                self._iter_by_pid[pid] = self.model.append([pid] + values + [icon_pixbuf])

        for pid in list(self._iter_by_pid.keys()):
            if pid not in visible_records:
                self.model.remove(self._iter_by_pid.pop(pid))

        total_count = len(bundle.processes)
        if self._search_text:
            self.process_count_label.set_label(
                f"{len(visible_records)} of {total_count} processes"
            )
        else:
            self.process_count_label.set_label(f"{total_count} processes")

    # ------------------------------------------------------- context actions

    def _on_button_press(self, tree_view, event):
        if event.type != Gdk.EventType.BUTTON_PRESS or event.button != 3:
            return False
        path_info = tree_view.get_path_at_pos(int(event.x), int(event.y))
        if path_info is None:
            return False
        path = path_info[0]
        tree_view.get_selection().select_path(path)
        self._popup_menu_for_path(path, event)
        return True

    def _on_popup_menu(self, tree_view):
        model, tree_iter = tree_view.get_selection().get_selected()
        if tree_iter is None:
            return False
        self._popup_menu_for_path(model.get_path(tree_iter), None)
        return True

    def _popup_menu_for_path(self, path, event):
        row = self.model[path]
        menu = build_process_context_menu(
            row[COLUMN_PID], row[COLUMN_NAME], self.get_toplevel(), row[COLUMN_NICE]
        )
        if event is not None:
            menu.popup_at_pointer(event)
        else:
            menu.popup_at_widget(
                self.tree_view, Gdk.Gravity.CENTER, Gdk.Gravity.CENTER, None
            )
