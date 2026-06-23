"""Cairo-drawn history graphs.

``HistoryGraph`` renders one or more series of samples scrolling right to
left, in one of three styles so each section of the app has its own visual
identity:

  * LINE — thin line with a soft fill (GPU, network)
  * AREA — heavier filled area (CPU)
  * BARS — discrete vertical bars (RAM)

All chrome colours derive from the widget's style context, so the graphs
follow the GTK theme in both light and dark modes.
"""

import math
from collections import deque
from enum import Enum

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk


class GraphStyle(Enum):
    LINE = "line"
    AREA = "area"
    BARS = "bars"


_GRAPH_PADDING = 3.0
_CORNER_RADIUS = 6.0


def _rounded_rectangle_path(cairo_context, x, y, width, height, radius):
    cairo_context.new_sub_path()
    cairo_context.arc(x + width - radius, y + radius, radius, -math.pi / 2, 0)
    cairo_context.arc(x + width - radius, y + height - radius, radius, 0, math.pi / 2)
    cairo_context.arc(x + radius, y + height - radius, radius, math.pi / 2, math.pi)
    cairo_context.arc(x + radius, y + radius, radius, math.pi, 3 * math.pi / 2)
    cairo_context.close_path()


class HistoryGraph(Gtk.DrawingArea):
    def __init__(
        self,
        style=GraphStyle.LINE,
        series_count=1,
        history_length=150,
        fixed_maximum=100.0,
        minimum_autoscale_value=1.0,
        graph_height=70,
        tooltip_formatter=None,
    ):
        super().__init__()
        self._style = style
        self._history_length = history_length
        self._fixed_maximum = fixed_maximum  # None enables autoscaling
        self._minimum_autoscale_value = minimum_autoscale_value
        self._series = [deque(maxlen=history_length) for _ in range(series_count)]
        self._series_colors = [(0.5, 0.5, 0.5)] * series_count
        self._tooltip_formatter = tooltip_formatter

        self.set_size_request(-1, graph_height)
        self.connect("draw", self._on_draw)
        if tooltip_formatter is not None:
            self.set_has_tooltip(True)
            self.connect("query-tooltip", self._on_query_tooltip)

    # ------------------------------------------------------------------- API

    def append_sample(self, values):
        if not isinstance(values, (list, tuple)):
            values = [values]
        for series, value in zip(self._series, values):
            series.append(max(0.0, float(value)))
        self.queue_draw()

    def set_series_colors(self, colors):
        self._series_colors = list(colors)
        self.queue_draw()

    def set_graph_height(self, height):
        self.set_size_request(-1, height)

    # ------------------------------------------------------------------ draw

    def _current_maximum(self):
        if self._fixed_maximum is not None:
            return self._fixed_maximum
        observed_maximum = max(
            (max(series) for series in self._series if series), default=0.0
        )
        return max(observed_maximum * 1.15, self._minimum_autoscale_value)

    def _on_draw(self, _widget, cairo_context):
        width = self.get_allocated_width()
        height = self.get_allocated_height()
        foreground = self.get_style_context().get_color(Gtk.StateFlags.NORMAL)

        # Frame and background.
        _rounded_rectangle_path(cairo_context, 0.5, 0.5, width - 1, height - 1, _CORNER_RADIUS)
        cairo_context.set_source_rgba(foreground.red, foreground.green, foreground.blue, 0.04)
        cairo_context.fill_preserve()
        cairo_context.set_source_rgba(foreground.red, foreground.green, foreground.blue, 0.12)
        cairo_context.set_line_width(1.0)
        cairo_context.stroke()

        # Horizontal grid lines at 25/50/75%.
        cairo_context.set_source_rgba(foreground.red, foreground.green, foreground.blue, 0.07)
        for fraction in (0.25, 0.5, 0.75):
            grid_y = round(_GRAPH_PADDING + (height - 2 * _GRAPH_PADDING) * fraction) + 0.5
            cairo_context.move_to(_GRAPH_PADDING, grid_y)
            cairo_context.line_to(width - _GRAPH_PADDING, grid_y)
        cairo_context.stroke()

        maximum_value = self._current_maximum()
        if maximum_value <= 0:
            return

        # Clip data drawing to the rounded interior.
        cairo_context.save()
        _rounded_rectangle_path(cairo_context, 1, 1, width - 2, height - 2, _CORNER_RADIUS - 1)
        cairo_context.clip()

        drawable_width = width - 2 * _GRAPH_PADDING
        drawable_height = height - 2 * _GRAPH_PADDING
        sample_step = drawable_width / max(self._history_length - 1, 1)

        for series, color in zip(self._series, self._series_colors):
            if len(series) >= 1:
                self._draw_series(
                    cairo_context, list(series), color, maximum_value,
                    width, height, sample_step, drawable_height,
                )
        cairo_context.restore()

    def _draw_series(self, cairo_context, samples, color, maximum_value,
                     width, height, sample_step, drawable_height):
        red, green, blue = color
        baseline_y = height - _GRAPH_PADDING

        def point_for_sample(index_from_newest, value):
            x = width - _GRAPH_PADDING - index_from_newest * sample_step
            y = baseline_y - min(value / maximum_value, 1.0) * drawable_height
            return x, y

        if self._style is GraphStyle.BARS:
            bar_width = max(sample_step * 0.65, 1.0)
            cairo_context.set_source_rgba(red, green, blue, 0.85)
            for index_from_newest, value in enumerate(reversed(samples)):
                x, y = point_for_sample(index_from_newest, value)
                bar_height = baseline_y - y
                if bar_height < 0.75:
                    bar_height = 0.75  # idle samples still leave a visible tick
                cairo_context.rectangle(x - bar_width / 2, baseline_y - bar_height, bar_width, bar_height)
            cairo_context.fill()
            return

        points = [
            point_for_sample(index_from_newest, value)
            for index_from_newest, value in enumerate(reversed(samples))
        ]

        fill_alpha = 0.32 if self._style is GraphStyle.AREA else 0.14
        line_width = 1.3 if self._style is GraphStyle.AREA else 1.6

        # Fill under the curve.
        cairo_context.move_to(points[0][0], baseline_y)
        for x, y in points:
            cairo_context.line_to(x, y)
        cairo_context.line_to(points[-1][0], baseline_y)
        cairo_context.close_path()
        cairo_context.set_source_rgba(red, green, blue, fill_alpha)
        cairo_context.fill()

        # The curve itself.
        cairo_context.move_to(*points[0])
        for x, y in points[1:]:
            cairo_context.line_to(x, y)
        cairo_context.set_source_rgba(red, green, blue, 0.95)
        cairo_context.set_line_width(line_width)
        cairo_context.set_line_join(1)  # CAIRO_LINE_JOIN_ROUND
        cairo_context.stroke()

    # --------------------------------------------------------------- tooltip

    def _on_query_tooltip(self, _widget, x, _y, _keyboard_mode, tooltip):
        width = self.get_allocated_width()
        sample_step = (width - 2 * _GRAPH_PADDING) / max(self._history_length - 1, 1)
        index_from_newest = int(round((width - _GRAPH_PADDING - x) / sample_step))

        values_at_cursor = []
        for series in self._series:
            samples = list(series)
            if 0 <= index_from_newest < len(samples):
                values_at_cursor.append(samples[len(samples) - 1 - index_from_newest])
            else:
                return False
        if not values_at_cursor:
            return False
        tooltip.set_text(self._tooltip_formatter(values_at_cursor))
        return True


class PerCoreBarChart(Gtk.DrawingArea):
    """A row of vertical bars, one per CPU core, with hover tooltips."""

    def __init__(self, chart_height=40):
        super().__init__()
        self._core_percentages = []
        self._bar_color = (0.5, 0.5, 0.5)
        self.set_size_request(-1, chart_height)
        self.set_has_tooltip(True)
        self.connect("draw", self._on_draw)
        self.connect("query-tooltip", self._on_query_tooltip)

    def set_core_percentages(self, percentages):
        self._core_percentages = list(percentages)
        self.queue_draw()

    def set_bar_color(self, color):
        self._bar_color = color
        self.queue_draw()

    def _bar_geometry(self, width):
        core_count = max(len(self._core_percentages), 1)
        gap = 3.0
        bar_width = max((width - gap * (core_count - 1)) / core_count, 2.0)
        return bar_width, gap

    def _on_draw(self, _widget, cairo_context):
        if not self._core_percentages:
            return
        width = self.get_allocated_width()
        height = self.get_allocated_height()
        foreground = self.get_style_context().get_color(Gtk.StateFlags.NORMAL)
        bar_width, gap = self._bar_geometry(width)
        red, green, blue = self._bar_color

        for core_index, core_percent in enumerate(self._core_percentages):
            bar_x = core_index * (bar_width + gap)
            # Background track.
            cairo_context.set_source_rgba(foreground.red, foreground.green, foreground.blue, 0.08)
            cairo_context.rectangle(bar_x, 0, bar_width, height)
            cairo_context.fill()
            # Filled portion, bottom up. Always at least a sliver so idle
            # cores remain visible.
            fill_height = max(height * min(core_percent, 100.0) / 100.0, 1.5)
            cairo_context.set_source_rgba(red, green, blue, 0.9)
            cairo_context.rectangle(bar_x, height - fill_height, bar_width, fill_height)
            cairo_context.fill()

    def _on_query_tooltip(self, _widget, x, _y, _keyboard_mode, tooltip):
        if not self._core_percentages:
            return False
        width = self.get_allocated_width()
        bar_width, gap = self._bar_geometry(width)
        core_index = int(x / (bar_width + gap))
        if 0 <= core_index < len(self._core_percentages):
            tooltip.set_text(f"Core {core_index} — {self._core_percentages[core_index]:.0f}%")
            return True
        return False
