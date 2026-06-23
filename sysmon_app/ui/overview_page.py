"""The Overview page: GPU, Memory, CPU, Network, and Disk section cards.

Each section is a self-contained card that knows how to render one snapshot
bundle. Sections expose:

  * ``update(bundle)``        — apply a fresh snapshot
  * ``set_compact(compact)``  — corner-of-the-screen mode (graphs shrink,
                                detail rows hide)
  * ``refresh_palette(dark)`` — re-derive graph colours after a theme change
"""

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, Pango

from ..formatting import (
    format_frequency_from_megahertz,
    format_percent,
    format_power_watts,
    format_rate,
    format_size,
    format_temperature_celsius,
)
from ..theming import GraphPalette
from ..widgets.history_graph import GraphStyle, HistoryGraph, PerCoreBarChart
from ..widgets.top_process_list import TopProcessEntry, TopProcessList

_FULL_GRAPH_HEIGHT = 66
_COMPACT_GRAPH_HEIGHT = 36


class StatGrid(Gtk.Grid):
    """A small two-column grid of labelled values inside a section card."""

    def __init__(self):
        super().__init__()
        self.set_column_spacing(14)
        self.set_row_spacing(2)
        self.set_column_homogeneous(True)
        self._value_labels = {}
        self._stat_count = 0

    def add_stat(self, stat_key, stat_title):
        pair_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        title_label = Gtk.Label(label=stat_title)
        title_label.get_style_context().add_class("dim-small-label")
        title_label.set_xalign(0.0)
        value_label = Gtk.Label(label="—")
        value_label.get_style_context().add_class("stat-value-label")
        value_label.set_xalign(1.0)
        pair_box.pack_start(title_label, False, False, 0)
        pair_box.pack_end(value_label, True, True, 0)

        column = self._stat_count % 2
        row = self._stat_count // 2
        self.attach(pair_box, column, row, 1, 1)
        self._value_labels[stat_key] = value_label
        self._stat_count += 1

    def set_stat(self, stat_key, formatted_value):
        self._value_labels[stat_key].set_label(formatted_value)


class SectionCard(Gtk.Box):
    """Shared card chrome: title row with a right-aligned headline value."""

    def __init__(self, section_title):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        self.get_style_context().add_class("metric-card")

        header_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        title_label = Gtk.Label(label=section_title)
        title_label.get_style_context().add_class("section-title")
        title_label.set_xalign(0.0)
        header_box.pack_start(title_label, False, False, 0)

        self.subtitle_label = Gtk.Label(label="")
        self.subtitle_label.get_style_context().add_class("dim-small-label")
        self.subtitle_label.set_ellipsize(Pango.EllipsizeMode.END)
        self.subtitle_label.set_xalign(0.0)
        header_box.pack_start(self.subtitle_label, True, True, 0)

        # pack_end order: first packed ends up rightmost.
        # A toggle: pressed (accent-tinted) while the section is popped out;
        # untoggling brings it home. Styled via the .pop-out-toggle CSS class.
        self.pop_out_button = Gtk.ToggleButton()
        self.pop_out_button.set_image(
            Gtk.Image.new_from_icon_name("window-new-symbolic", Gtk.IconSize.MENU)
        )
        self.pop_out_button.set_relief(Gtk.ReliefStyle.NONE)
        self.pop_out_button.set_focus_on_click(False)
        self.pop_out_button.set_valign(Gtk.Align.CENTER)
        self.pop_out_button.get_style_context().add_class("pop-out-toggle")
        self.pop_out_button.set_tooltip_text("Pop out into its own window")
        header_box.pack_end(self.pop_out_button, False, False, 0)

        self.headline_label = Gtk.Label(label="—")
        self.headline_label.get_style_context().add_class("headline-value")
        self.headline_label.set_xalign(1.0)
        header_box.pack_end(self.headline_label, False, False, 0)

        self.pack_start(header_box, False, False, 0)

    def set_popped_out(self, popped_out):
        self.pop_out_button.set_tooltip_text(
            "Return to the main window" if popped_out else "Pop out into its own window"
        )

    def set_compact(self, compact):
        raise NotImplementedError

    def refresh_palette(self, is_dark_theme):
        raise NotImplementedError


class GpuSection(SectionCard):
    def __init__(self):
        super().__init__("GPU")
        self.graph = HistoryGraph(
            style=GraphStyle.LINE,
            fixed_maximum=100.0,
            graph_height=_FULL_GRAPH_HEIGHT,
            tooltip_formatter=lambda values: f"{values[0]:.0f}% busy",
        )
        self.pack_start(self.graph, False, False, 0)

        vram_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        vram_title = Gtk.Label(label="VRAM")
        vram_title.get_style_context().add_class("dim-small-label")
        vram_box.pack_start(vram_title, False, False, 0)
        self.vram_level_bar = Gtk.LevelBar()
        self.vram_level_bar.set_min_value(0.0)
        self.vram_level_bar.set_max_value(1.0)
        self.vram_level_bar.set_valign(Gtk.Align.CENTER)
        vram_box.pack_start(self.vram_level_bar, True, True, 0)
        self.vram_text_label = Gtk.Label(label="—")
        self.vram_text_label.get_style_context().add_class("stat-value-label")
        vram_box.pack_end(self.vram_text_label, False, False, 0)
        self.pack_start(vram_box, False, False, 0)

        self.stats = StatGrid()
        self.stats.add_stat("core_clock", "Core")
        self.stats.add_stat("memory_clock", "VRAM Clk")
        self.stats.add_stat("power", "Power")
        self.stats.add_stat("junction", "Hot Spot")
        self.stats.add_stat("fan", "Fan")
        self.pack_start(self.stats, False, False, 0)

        self.top_processes = TopProcessList()
        self.pack_start(self.top_processes, False, False, 0)

    def update(self, bundle):
        gpu = bundle.gpu
        if gpu is None:
            self.headline_label.set_label("no AMD GPU")
            return
        self.subtitle_label.set_label(gpu.device_name)

        headline = format_percent(gpu.busy_percent)
        if gpu.temperature_edge_celsius is not None:
            headline += f" · {format_temperature_celsius(gpu.temperature_edge_celsius)}"
        self.headline_label.set_label(headline)
        self.graph.append_sample(gpu.busy_percent)

        if gpu.vram_total_bytes > 0:
            self.vram_level_bar.set_value(gpu.vram_used_bytes / gpu.vram_total_bytes)
            self.vram_text_label.set_label(
                f"{format_size(gpu.vram_used_bytes)} / {format_size(gpu.vram_total_bytes)}"
            )

        self.stats.set_stat("core_clock", format_frequency_from_megahertz(gpu.core_clock_megahertz))
        self.stats.set_stat("memory_clock", format_frequency_from_megahertz(gpu.memory_clock_megahertz))
        power_text = format_power_watts(gpu.power_draw_watts)
        if gpu.power_cap_watts:
            power_text += f" / {format_power_watts(gpu.power_cap_watts)}"
        self.stats.set_stat("power", power_text)
        self.stats.set_stat("junction", format_temperature_celsius(gpu.temperature_junction_celsius))
        fan_rpm = gpu.fan_speed_rpm
        self.stats.set_stat("fan", f"{fan_rpm} rpm" if fan_rpm else "0 rpm")

        top_entries = []
        for gpu_usage in gpu.processes[:3]:
            process_record = bundle.processes.get(gpu_usage.pid)
            process_name = process_record.name if process_record else f"pid {gpu_usage.pid}"
            nice_value = process_record.nice_value if process_record else None
            top_entries.append(
                TopProcessEntry(
                    pid=gpu_usage.pid,
                    display_name=process_name,
                    formatted_value=(
                        f"{gpu_usage.busy_percent:.0f}% · {format_size(gpu_usage.vram_bytes)}"
                    ),
                    nice_value=nice_value,
                )
            )
        self.top_processes.update_entries(top_entries)

    def set_compact(self, compact):
        self.graph.set_graph_height(_COMPACT_GRAPH_HEIGHT if compact else _FULL_GRAPH_HEIGHT)
        self.stats.set_visible(not compact)
        self.top_processes.set_visible(not compact)
        self.stats.set_no_show_all(compact)
        self.top_processes.set_no_show_all(compact)

    def refresh_palette(self, is_dark_theme):
        self.graph.set_series_colors([GraphPalette.color("gpu", is_dark_theme)])


class MemorySection(SectionCard):
    def __init__(self):
        super().__init__("Memory")
        self.graph = HistoryGraph(
            style=GraphStyle.BARS,
            history_length=60,
            fixed_maximum=100.0,
            graph_height=_FULL_GRAPH_HEIGHT,
            tooltip_formatter=lambda values: f"{values[0]:.0f}% used",
        )
        self.pack_start(self.graph, False, False, 0)

        self.stats = StatGrid()
        self.stats.add_stat("used", "Used")
        self.stats.add_stat("available", "Available")
        self.stats.add_stat("cached", "Cached")
        self.stats.add_stat("swap", "Swap")
        self.pack_start(self.stats, False, False, 0)

        self.top_processes = TopProcessList()
        self.pack_start(self.top_processes, False, False, 0)

    def update(self, bundle):
        memory = bundle.memory
        if memory is None:
            return
        self.headline_label.set_label(
            f"{format_percent(memory.used_percent)} · "
            f"{format_size(memory.used_bytes)} / {format_size(memory.total_bytes)}"
        )
        self.graph.append_sample(memory.used_percent)
        self.stats.set_stat("used", format_size(memory.used_bytes))
        self.stats.set_stat("available", format_size(memory.available_bytes))
        self.stats.set_stat("cached", format_size(memory.cached_bytes))
        swap_text = (
            f"{format_size(memory.swap_used_bytes)} / {format_size(memory.swap_total_bytes)}"
            if memory.swap_total_bytes
            else "none"
        )
        self.stats.set_stat("swap", swap_text)

        self.top_processes.update_entries(
            [
                TopProcessEntry(
                    pid=record.pid,
                    display_name=record.name,
                    formatted_value=format_size(record.resident_memory_bytes),
                    nice_value=record.nice_value,
                )
                for record in bundle.top_processes_by("resident_memory_bytes")
            ]
        )

    def set_compact(self, compact):
        self.graph.set_graph_height(_COMPACT_GRAPH_HEIGHT if compact else _FULL_GRAPH_HEIGHT)
        self.stats.set_visible(not compact)
        self.top_processes.set_visible(not compact)
        self.stats.set_no_show_all(compact)
        self.top_processes.set_no_show_all(compact)

    def refresh_palette(self, is_dark_theme):
        self.graph.set_series_colors([GraphPalette.color("memory", is_dark_theme)])


class CpuSection(SectionCard):
    def __init__(self):
        super().__init__("CPU")
        self.graph = HistoryGraph(
            style=GraphStyle.AREA,
            fixed_maximum=100.0,
            graph_height=_FULL_GRAPH_HEIGHT,
            tooltip_formatter=lambda values: f"{values[0]:.0f}% overall",
        )
        self.pack_start(self.graph, False, False, 0)

        self.per_core_chart = PerCoreBarChart()
        self.pack_start(self.per_core_chart, False, False, 0)

        self.stats = StatGrid()
        self.stats.add_stat("load", "Load")
        self.stats.add_stat("tasks", "Tasks")
        self.pack_start(self.stats, False, False, 0)

        self.top_processes = TopProcessList()
        self.pack_start(self.top_processes, False, False, 0)

    def update(self, bundle):
        cpu = bundle.cpu
        if cpu is None:
            return
        headline = format_percent(cpu.overall_percent)
        if cpu.frequency_megahertz is not None:
            headline += f" · {format_frequency_from_megahertz(cpu.frequency_megahertz)}"
        self.headline_label.set_label(headline)
        self.subtitle_label.set_label(f"{cpu.core_count} threads")

        self.graph.append_sample(cpu.overall_percent)
        self.per_core_chart.set_core_percentages(cpu.per_core_percent)
        self.stats.set_stat(
            "load",
            f"{cpu.load_average_1m:.2f} · {cpu.load_average_5m:.2f} · {cpu.load_average_15m:.2f}",
        )
        self.stats.set_stat("tasks", str(len(bundle.processes)))

        self.top_processes.update_entries(
            [
                TopProcessEntry(
                    pid=record.pid,
                    display_name=record.name,
                    formatted_value=f"{record.cpu_percent:.1f}%",
                    nice_value=record.nice_value,
                )
                for record in bundle.top_processes_by("cpu_percent")
            ]
        )

    def set_compact(self, compact):
        self.graph.set_graph_height(_COMPACT_GRAPH_HEIGHT if compact else _FULL_GRAPH_HEIGHT)
        for widget in (self.per_core_chart, self.stats, self.top_processes):
            widget.set_visible(not compact)
            widget.set_no_show_all(compact)

    def refresh_palette(self, is_dark_theme):
        cpu_color = GraphPalette.color("cpu", is_dark_theme)
        self.graph.set_series_colors([cpu_color])
        self.per_core_chart.set_bar_color(cpu_color)


class NetworkSection(SectionCard):
    def __init__(self):
        super().__init__("Network")
        self.graph = HistoryGraph(
            style=GraphStyle.LINE,
            series_count=2,
            fixed_maximum=None,  # autoscale to traffic
            minimum_autoscale_value=20 * 1024.0,
            graph_height=_FULL_GRAPH_HEIGHT,
            tooltip_formatter=lambda values: (
                f"↓ {format_rate(values[0])}   ↑ {format_rate(values[1])}"
            ),
        )
        self.pack_start(self.graph, False, False, 0)

        self.stats = StatGrid()
        self.stats.add_stat("total_down", "Total ↓")
        self.stats.add_stat("total_up", "Total ↑")
        self.pack_start(self.stats, False, False, 0)

        self.top_processes = TopProcessList()
        self.pack_start(self.top_processes, False, False, 0)

        self.unavailable_hint_label = Gtk.Label(label="")
        self.unavailable_hint_label.get_style_context().add_class("unavailable-hint")
        self.unavailable_hint_label.set_xalign(0.0)
        self.unavailable_hint_label.set_line_wrap(True)
        self.unavailable_hint_label.set_selectable(True)
        self.unavailable_hint_label.set_no_show_all(True)
        self.pack_start(self.unavailable_hint_label, False, False, 0)
        self._hint_applied = False
        self._compact = False

    def update(self, bundle):
        network = bundle.network
        if network is None:
            return
        self.headline_label.set_label(
            f"↓ {format_rate(network.download_bytes_per_second)}   "
            f"↑ {format_rate(network.upload_bytes_per_second)}"
        )
        self.graph.append_sample(
            [network.download_bytes_per_second, network.upload_bytes_per_second]
        )
        self.stats.set_stat("total_down", format_size(network.total_received_bytes))
        self.stats.set_stat("total_up", format_size(network.total_sent_bytes))

        if network.per_process_bytes_per_second is not None:
            self.top_processes.update_entries(
                [
                    TopProcessEntry(
                        pid=record.pid,
                        display_name=record.name,
                        formatted_value=format_rate(record.network_bytes_per_second),
                        nice_value=record.nice_value,
                    )
                    for record in bundle.top_processes_by("network_bytes_per_second")
                ]
            )
        elif not self._hint_applied and network.per_process_unavailable_hint:
            self.unavailable_hint_label.set_label(network.per_process_unavailable_hint)
            self.top_processes.set_visible(False)
            self.top_processes.set_no_show_all(True)
            self.unavailable_hint_label.set_visible(not self._compact)
            self._hint_applied = True

    def set_compact(self, compact):
        self._compact = compact
        self.graph.set_graph_height(_COMPACT_GRAPH_HEIGHT if compact else _FULL_GRAPH_HEIGHT)
        self.stats.set_visible(not compact)
        self.stats.set_no_show_all(compact)
        if self._hint_applied:
            self.unavailable_hint_label.set_visible(not compact)
        else:
            self.top_processes.set_visible(not compact)
            self.top_processes.set_no_show_all(compact)

    def refresh_palette(self, is_dark_theme):
        self.graph.set_series_colors(
            [
                GraphPalette.color("network_download", is_dark_theme),
                GraphPalette.color("network_upload", is_dark_theme),
            ]
        )


class DiskSection(SectionCard):
    """Mounted drives with live read/write speeds — deliberately graph-free."""

    def __init__(self):
        super().__init__("Disks")
        self._rows_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        self.pack_start(self._rows_box, False, False, 0)
        self._rows_by_mount_point = {}
        self._compact = False

    def update(self, bundle):
        drives = bundle.disks
        current_mount_points = [drive.mount_point for drive in drives]
        if current_mount_points != list(self._rows_by_mount_point.keys()):
            self._rebuild_rows(drives)

        total_read = sum(drive.read_bytes_per_second for drive in drives)
        total_write = sum(drive.write_bytes_per_second for drive in drives)
        self.headline_label.set_label(
            f"R {format_rate(total_read)}   W {format_rate(total_write)}"
        )

        for drive in drives:
            row = self._rows_by_mount_point.get(drive.mount_point)
            if row is None:
                continue
            row["read_label"].set_label(f"R {format_rate(drive.read_bytes_per_second)}")
            row["write_label"].set_label(f"W {format_rate(drive.write_bytes_per_second)}")
            row["detail_label"].set_label(
                f"{drive.mount_point} · {format_size(drive.used_bytes)} of "
                f"{format_size(drive.total_bytes)}"
            )

    def _rebuild_rows(self, drives):
        for child in self._rows_box.get_children():
            self._rows_box.remove(child)
        self._rows_by_mount_point = {}

        for drive in drives:
            row_grid = Gtk.Grid()
            row_grid.set_column_spacing(10)

            name_label = Gtk.Label(label=drive.display_name)
            name_label.set_xalign(0.0)
            name_label.set_ellipsize(Pango.EllipsizeMode.END)
            name_label.set_hexpand(True)
            name_label.set_tooltip_text(drive.device_path)
            row_grid.attach(name_label, 0, 0, 1, 1)

            read_label = Gtk.Label(label="R —")
            read_label.get_style_context().add_class("stat-value-label")
            read_label.set_xalign(1.0)
            row_grid.attach(read_label, 1, 0, 1, 1)

            write_label = Gtk.Label(label="W —")
            write_label.get_style_context().add_class("stat-value-label")
            write_label.set_xalign(1.0)
            row_grid.attach(write_label, 2, 0, 1, 1)

            detail_label = Gtk.Label(label=drive.mount_point)
            detail_label.get_style_context().add_class("dim-small-label")
            detail_label.set_xalign(0.0)
            detail_label.set_ellipsize(Pango.EllipsizeMode.END)
            row_grid.attach(detail_label, 0, 1, 3, 1)

            self._rows_box.pack_start(row_grid, False, False, 0)
            self._rows_by_mount_point[drive.mount_point] = {
                "read_label": read_label,
                "write_label": write_label,
                "detail_label": detail_label,
            }
        self._rows_box.show_all()
        if self._compact:
            self._apply_compact_to_rows()

    def _apply_compact_to_rows(self):
        for row in self._rows_by_mount_point.values():
            row["detail_label"].set_visible(not self._compact)
            row["detail_label"].set_no_show_all(self._compact)

    def set_compact(self, compact):
        # Compact keeps one line per drive: name + R/W speeds, details hidden.
        self._compact = compact
        self._apply_compact_to_rows()

    def refresh_palette(self, is_dark_theme):
        pass  # no graphs here


class OverviewPage(Gtk.ScrolledWindow):
    def __init__(self):
        super().__init__()
        self.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)

        self._sections_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        self._sections_box.set_margin_top(8)
        self._sections_box.set_margin_bottom(8)
        self._sections_box.set_margin_start(8)
        self._sections_box.set_margin_end(8)

        # Ordered by how much you care about each one.
        self.sections = {
            "gpu": GpuSection(),
            "memory": MemorySection(),
            "cpu": CpuSection(),
            "network": NetworkSection(),
            "disks": DiskSection(),
        }
        self._section_order = list(self.sections.keys())
        for section in self.sections.values():
            self._sections_box.pack_start(section, False, False, 0)
        self.add(self._sections_box)

    def update(self, bundle):
        # Includes popped-out sections: they stay in ``self.sections`` even
        # while living in their own window.
        for section in self.sections.values():
            if section.get_visible():
                section.update(bundle)

    def detach_section(self, section_key):
        """Remove a section card so it can live in a pop-out window."""
        section = self.sections[section_key]
        self._sections_box.remove(section)
        return section

    def reattach_section(self, section_key):
        """Return a popped-out section card to its original position."""
        section = self.sections[section_key]
        attached_children = self._sections_box.get_children()
        sections_before = [
            self.sections[key]
            for key in self._section_order[: self._section_order.index(section_key)]
        ]
        target_position = sum(1 for child in attached_children if child in sections_before)
        self._sections_box.pack_start(section, False, False, 0)
        self._sections_box.reorder_child(section, target_position)

    def set_section_visible(self, section_key, visible):
        section = self.sections.get(section_key)
        if section is not None:
            section.set_visible(visible)
            section.set_no_show_all(not visible)

    def set_compact(self, compact):
        for section in self.sections.values():
            section.set_compact(compact)

    def refresh_palette(self, is_dark_theme):
        for section in self.sections.values():
            section.refresh_palette(is_dark_theme)
