"""The main window: header bar with Overview/Processes switcher, the
settings popover, pop-out window management, and wiring between the sampler
thread and the pages."""

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, GLib, Gtk

from ..formatting import set_binary_units
from ..monitors.sampler import SystemSamplerThread
from ..settings import (
    SECTION_KEYS,
    THEME_MODE_BLOSSOM,
    THEME_MODE_DARK,
    THEME_MODE_FOLLOW_SYSTEM,
    THEME_MODE_FUNKY,
    THEME_MODE_LIGHT,
    UserSettings,
)
from ..theming import (
    apply_theme_mode,
    available_graph_palettes,
    set_graph_palette,
    widget_uses_dark_theme,
)
from .overview_page import OverviewPage
from .popout_window import SectionPopoutWindow
from .processes_page import ProcessesPage

_SECTION_DISPLAY_NAMES = {
    "gpu": "GPU",
    "memory": "Memory",
    "cpu": "CPU",
    "network": "Network",
    "disks": "Disks",
}

_UPDATE_INTERVAL_CHOICES = [1.0, 2.0, 3.0, 5.0]

# How often to check whether a dragged pop-out has been dropped (released).
_POPOUT_DROP_POLL_MILLISECONDS = 90

# Themes that bring a matching graph palette along when selected (the palette
# stays freely changeable afterwards).
_THEME_COMPANION_PALETTES = {
    THEME_MODE_BLOSSOM: "blossom",
    THEME_MODE_FUNKY: "funky",
}


class SysMonWindow(Gtk.ApplicationWindow):
    def __init__(self, application):
        super().__init__(application=application, title="SysMon")
        self.settings = UserSettings.load()
        self.set_default_size(self.settings.window_width, self.settings.window_height)
        self.set_size_request(330, 380)

        self._latest_bundle = None
        self._popout_windows = {}  # section key -> SectionPopoutWindow
        self._suppress_setting_callbacks = True  # while building controls
        self._suppress_popout_callbacks = False  # while syncing toggle state
        self._main_window_iconified = False
        self.connect("window-state-event", self._on_window_state_event)

        apply_theme_mode(self.settings.theme_mode)
        set_graph_palette(self.settings.graph_palette)
        set_binary_units(self.settings.use_binary_units)
        self._popouts_awaiting_drop = set()  # section keys mid-drag
        self._build_pages()
        self._build_header_bar()
        self._apply_settings_to_ui()
        self._suppress_setting_callbacks = False

        self.connect("style-updated", self._on_style_updated)
        self.connect("destroy", self._on_destroy)

        self.sampler = SystemSamplerThread(
            self._on_snapshot_ready, self.settings.update_interval_seconds
        )
        self.sampler.start()

        self.show_all()
        self.pin_button.set_visible(self.settings.show_pin_button)
        if self.settings.always_on_top:
            self.set_keep_above(True)
        self._refresh_graph_palette()

        # Restore sections that were popped out when the app last closed.
        for section_key in list(self.settings.popped_out_sections):
            self._pop_out_section(section_key, persist=False)

    # ----------------------------------------------------------------- build

    def _build_pages(self):
        self.stack = Gtk.Stack()
        self.stack.set_transition_type(Gtk.StackTransitionType.CROSSFADE)
        self.stack.set_transition_duration(150)

        self.overview_page = OverviewPage()
        self.processes_page = ProcessesPage()
        self.stack.add_titled(self.overview_page, "overview", "Overview")
        self.stack.add_titled(self.processes_page, "processes", "Processes")
        self.stack.connect("notify::visible-child", self._on_page_switched)
        self.add(self.stack)

        for section_key, section in self.overview_page.sections.items():
            section.pop_out_button.connect(
                "toggled", self._on_pop_out_toggled, section_key
            )

    def _build_header_bar(self):
        header_bar = Gtk.HeaderBar()
        header_bar.set_show_close_button(True)

        stack_switcher = Gtk.StackSwitcher()
        stack_switcher.set_stack(self.stack)
        header_bar.set_custom_title(stack_switcher)

        self.pause_button = Gtk.ToggleButton()
        self.pause_button.set_image(
            Gtk.Image.new_from_icon_name("media-playback-pause-symbolic", Gtk.IconSize.BUTTON)
        )
        self.pause_button.set_tooltip_text("Pause updates")
        self.pause_button.connect("toggled", self._on_pause_toggled)
        header_bar.pack_start(self.pause_button)

        self.pin_button = Gtk.ToggleButton()
        self.pin_button.set_image(
            Gtk.Image.new_from_icon_name("view-pin-symbolic", Gtk.IconSize.BUTTON)
        )
        self.pin_button.set_tooltip_text("Keep window on top")
        self.pin_button.get_style_context().add_class("pin-toggle")
        self.pin_button.set_no_show_all(True)  # visibility driven by settings
        self.pin_button.connect("toggled", self._on_pin_button_toggled)
        header_bar.pack_start(self.pin_button)

        menu_button = Gtk.MenuButton()
        menu_button.set_image(
            Gtk.Image.new_from_icon_name("open-menu-symbolic", Gtk.IconSize.BUTTON)
        )
        menu_button.set_tooltip_text("Display options")
        menu_button.set_popover(self._build_settings_popover())
        header_bar.pack_end(menu_button)

        self.set_titlebar(header_bar)

    def _build_settings_popover(self):
        popover_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        popover_box.set_margin_top(10)
        popover_box.set_margin_bottom(10)
        popover_box.set_margin_start(12)
        popover_box.set_margin_end(12)

        def add_heading(text, top_margin=8):
            heading = Gtk.Label(label=text)
            heading.get_style_context().add_class("dim-small-label")
            heading.set_xalign(0.0)
            heading.set_margin_top(top_margin)
            popover_box.pack_start(heading, False, False, 0)

        # Appearance -----------------------------------------------------
        add_heading("Appearance", top_margin=0)
        self.theme_radio_buttons = {}
        previous_radio = None
        for theme_mode, display_name in [
            (THEME_MODE_FOLLOW_SYSTEM, "Follow system theme"),
            (THEME_MODE_LIGHT, "Light"),
            (THEME_MODE_DARK, "Dark"),
            (THEME_MODE_BLOSSOM, "Blossom (AMOLED)"),
            (THEME_MODE_FUNKY, "Funky Pink"),
        ]:
            radio = Gtk.RadioButton.new_with_label_from_widget(previous_radio, display_name)
            radio.connect("toggled", self._on_theme_radio_toggled, theme_mode)
            popover_box.pack_start(radio, False, False, 0)
            self.theme_radio_buttons[theme_mode] = radio
            previous_radio = radio

        # Graph colours ----------------------------------------------------
        add_heading("Graph colours")
        palette_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        palette_box_second_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        self.palette_radio_buttons = {}
        previous_radio = None
        for index, (palette_key, display_name) in enumerate(available_graph_palettes()):
            radio = Gtk.RadioButton.new_with_label_from_widget(previous_radio, display_name)
            radio.connect("toggled", self._on_palette_radio_toggled, palette_key)
            target_row = palette_box if index < 4 else palette_box_second_row
            target_row.pack_start(radio, False, False, 4)
            self.palette_radio_buttons[palette_key] = radio
            previous_radio = radio
        popover_box.pack_start(palette_box, False, False, 0)
        popover_box.pack_start(palette_box_second_row, False, False, 0)

        # Units --------------------------------------------------------------
        add_heading("Units")
        units_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        self.units_radio_buttons = {}
        previous_radio = None
        for use_binary_units, display_name in [
            (False, "Decimal (GB)"),
            (True, "Binary (GiB)"),
        ]:
            radio = Gtk.RadioButton.new_with_label_from_widget(previous_radio, display_name)
            radio.set_tooltip_text(
                "Binary matches htop and GNOME System Monitor; "
                "decimal matches drive stickers and ISP speeds"
            )
            radio.connect("toggled", self._on_units_radio_toggled, use_binary_units)
            units_box.pack_start(radio, False, False, 4)
            self.units_radio_buttons[use_binary_units] = radio
            previous_radio = radio
        popover_box.pack_start(units_box, False, False, 0)

        # Update interval --------------------------------------------------
        add_heading("Update interval")
        interval_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        self.interval_radio_buttons = {}
        previous_radio = None
        for interval_seconds in _UPDATE_INTERVAL_CHOICES:
            radio = Gtk.RadioButton.new_with_label_from_widget(
                previous_radio, f"{interval_seconds:g}s"
            )
            radio.connect("toggled", self._on_interval_radio_toggled, interval_seconds)
            interval_box.pack_start(radio, False, False, 4)
            self.interval_radio_buttons[interval_seconds] = radio
            previous_radio = radio
        popover_box.pack_start(interval_box, False, False, 0)

        # Window behaviour --------------------------------------------------
        add_heading("Window")
        self.always_on_top_check = Gtk.CheckButton(label="Always on top")
        self.always_on_top_check.connect("toggled", self._on_always_on_top_toggled)
        popover_box.pack_start(self.always_on_top_check, False, False, 0)

        self.show_pin_check = Gtk.CheckButton(label="Show pin button")
        self.show_pin_check.set_tooltip_text(
            "A header-bar pin for toggling always-on-top in one click"
        )
        self.show_pin_check.connect("toggled", self._on_show_pin_toggled)
        popover_box.pack_start(self.show_pin_check, False, False, 0)

        self.compact_mode_check = Gtk.CheckButton(label="Compact mode")
        self.compact_mode_check.set_tooltip_text(
            "Shrink graphs and hide detail rows — ideal for a corner of the screen"
        )
        self.compact_mode_check.connect("toggled", self._on_compact_mode_toggled)
        popover_box.pack_start(self.compact_mode_check, False, False, 0)

        # Visible sections ---------------------------------------------------
        add_heading("Overview sections")
        self.section_check_buttons = {}
        for section_key in SECTION_KEYS:
            check = Gtk.CheckButton(label=_SECTION_DISPLAY_NAMES[section_key])
            check.connect("toggled", self._on_section_toggled, section_key)
            popover_box.pack_start(check, False, False, 0)
            self.section_check_buttons[section_key] = check

        popover_box.show_all()
        popover = Gtk.Popover()
        popover.add(popover_box)
        return popover

    def _apply_settings_to_ui(self):
        self.theme_radio_buttons[self.settings.theme_mode].set_active(True)
        palette_radio = self.palette_radio_buttons.get(self.settings.graph_palette)
        if palette_radio is not None:
            palette_radio.set_active(True)
        self.units_radio_buttons[bool(self.settings.use_binary_units)].set_active(True)
        nearest_interval = min(
            _UPDATE_INTERVAL_CHOICES,
            key=lambda choice: abs(choice - self.settings.update_interval_seconds),
        )
        self.interval_radio_buttons[nearest_interval].set_active(True)
        self.always_on_top_check.set_active(self.settings.always_on_top)
        self.pin_button.set_active(self.settings.always_on_top)
        self.show_pin_check.set_active(self.settings.show_pin_button)
        self.compact_mode_check.set_active(self.settings.compact_mode)
        for section_key, check in self.section_check_buttons.items():
            check.set_active(self.settings.visible_sections.get(section_key, True))
            self.overview_page.set_section_visible(
                section_key, self.settings.visible_sections.get(section_key, True)
            )
        self.overview_page.set_compact(self.settings.compact_mode)

    # --------------------------------------------------------------- popouts

    def _on_pop_out_toggled(self, button, section_key):
        if self._suppress_popout_callbacks:
            return
        if button.get_active():
            self._pop_out_section(section_key)
        else:
            popout = self._popout_windows.get(section_key)
            if popout is not None:
                popout.close()  # same path as the user closing the window

    def _set_pop_out_toggle_state(self, section, active):
        self._suppress_popout_callbacks = True
        section.pop_out_button.set_active(active)
        self._suppress_popout_callbacks = False
        section.set_popped_out(active)

    def _pop_out_section(self, section_key, persist=True):
        if section_key in self._popout_windows:
            return
        section = self.overview_page.detach_section(section_key)
        self._set_pop_out_toggle_state(section, True)
        popout = SectionPopoutWindow(
            section,
            section_key,
            _SECTION_DISPLAY_NAMES[section_key],
            self._on_popout_closed,
        )
        popout.connect("configure-event", self._on_popout_configure_event)
        self._popout_windows[section_key] = popout
        if persist:
            self._persist_popped_out_sections()

    def _on_popout_closed(self, popout):
        section = popout.release_section()
        self._popout_windows.pop(popout.section_key, None)
        self.overview_page.reattach_section(popout.section_key)
        self._set_pop_out_toggle_state(section, False)
        self._persist_popped_out_sections()

    def _on_popout_configure_event(self, popout, event):
        """Watch pop-out movement and dock the card when it is *dropped*
        onto the main window.

        Configure events stream continuously while the window manager moves
        the pop-out, so docking mid-drag would snap the window back the
        instant it crossed the main window — making it impossible to drag at
        all when it opened on top of us. Instead, a genuine move (not a
        resize, not the initial placement) starts a short poll that waits
        for the mouse button to be released and only then decides whether
        the drop landed on the main window.
        """
        previous_geometry = popout.last_known_geometry
        popout.last_known_geometry = (event.x, event.y, event.width, event.height)
        if previous_geometry is None or not popout.drag_dock_armed:
            return False
        was_moved = (event.x, event.y) != previous_geometry[:2]
        was_resized = (event.width, event.height) != previous_geometry[2:]
        if was_moved and not was_resized:
            self._watch_for_popout_drop(popout)
        return False

    def _watch_for_popout_drop(self, popout):
        if popout.section_key in self._popouts_awaiting_drop:
            return  # already polling this drag
        self._popouts_awaiting_drop.add(popout.section_key)
        GLib.timeout_add(
            _POPOUT_DROP_POLL_MILLISECONDS, self._check_popout_dropped, popout
        )

    def _check_popout_dropped(self, popout):
        """Poll until the drag's mouse button is released, then dock the
        pop-out if it was dropped on top of the main window."""
        main_gdk_window = self.get_window()
        if popout.section_key not in self._popout_windows or main_gdk_window is None:
            self._popouts_awaiting_drop.discard(popout.section_key)
            return False  # pop-out closed (or we are shutting down) mid-drag

        pointer = self.get_display().get_default_seat().get_pointer()
        _window, _x, _y, modifier_state = main_gdk_window.get_device_position(pointer)
        if modifier_state & Gdk.ModifierType.BUTTON1_MASK:
            return True  # still dragging — keep polling

        self._popouts_awaiting_drop.discard(popout.section_key)
        if self._main_window_iconified or not self.get_visible():
            return False

        _screen, pointer_x, pointer_y = pointer.get_position()
        main_x, main_y = self.get_position()
        main_width, main_height = self.get_size()
        dropped_on_main_window = (
            main_x <= pointer_x <= main_x + main_width
            and main_y <= pointer_y <= main_y + main_height
        )
        if dropped_on_main_window:
            popout.close()
        return False

    def _on_window_state_event(self, _window, event):
        self._main_window_iconified = bool(
            event.new_window_state & Gdk.WindowState.ICONIFIED
        )
        return False

    def _persist_popped_out_sections(self):
        self.settings.popped_out_sections = list(self._popout_windows.keys())
        self.settings.save()

    # ------------------------------------------------------------- callbacks

    def _on_snapshot_ready(self, bundle):
        self._latest_bundle = bundle
        self.overview_page.update(bundle)
        if self.stack.get_visible_child_name() == "processes":
            self.processes_page.update(bundle)
        return False  # one-shot idle callback

    def _on_page_switched(self, _stack, _parameter):
        # Bring the table up to date the moment it becomes visible.
        if (
            self.stack.get_visible_child_name() == "processes"
            and self._latest_bundle is not None
        ):
            self.processes_page.update(self._latest_bundle)

    def _on_pause_toggled(self, button):
        paused = button.get_active()
        self.sampler.set_paused(paused)
        button.set_tooltip_text("Resume updates" if paused else "Pause updates")

    def _on_pin_button_toggled(self, button):
        if self._suppress_setting_callbacks:
            return
        self._set_always_on_top(button.get_active())

    def _on_always_on_top_toggled(self, check):
        if self._suppress_setting_callbacks:
            return
        self._set_always_on_top(check.get_active())

    def _set_always_on_top(self, on_top):
        self.settings.always_on_top = on_top
        self.set_keep_above(on_top)
        self._suppress_setting_callbacks = True
        self.always_on_top_check.set_active(on_top)
        self.pin_button.set_active(on_top)
        self._suppress_setting_callbacks = False
        self.settings.save()

    def _on_show_pin_toggled(self, check):
        if self._suppress_setting_callbacks:
            return
        self.settings.show_pin_button = check.get_active()
        self.pin_button.set_visible(self.settings.show_pin_button)
        self.settings.save()

    def _on_theme_radio_toggled(self, radio, theme_mode):
        if self._suppress_setting_callbacks or not radio.get_active():
            return
        self.settings.theme_mode = theme_mode
        apply_theme_mode(theme_mode)
        self.settings.save()
        companion_palette = _THEME_COMPANION_PALETTES.get(theme_mode)
        if companion_palette is not None:
            self.palette_radio_buttons[companion_palette].set_active(True)
        self._refresh_graph_palette()

    def _on_palette_radio_toggled(self, radio, palette_key):
        if self._suppress_setting_callbacks or not radio.get_active():
            return
        self.settings.graph_palette = palette_key
        set_graph_palette(palette_key)
        self.settings.save()
        self._refresh_graph_palette()

    def _on_units_radio_toggled(self, radio, use_binary_units):
        if self._suppress_setting_callbacks or not radio.get_active():
            return
        self.settings.use_binary_units = use_binary_units
        set_binary_units(use_binary_units)
        self.settings.save()

    def _on_interval_radio_toggled(self, radio, interval_seconds):
        if self._suppress_setting_callbacks or not radio.get_active():
            return
        self.settings.update_interval_seconds = interval_seconds
        self.sampler.set_update_interval(interval_seconds)
        self.settings.save()

    def _on_compact_mode_toggled(self, check):
        if self._suppress_setting_callbacks:
            return
        self.settings.compact_mode = check.get_active()
        self.overview_page.set_compact(self.settings.compact_mode)
        self.settings.save()

    def _on_section_toggled(self, check, section_key):
        if self._suppress_setting_callbacks:
            return
        visible = check.get_active()
        if not visible and section_key in self._popout_windows:
            # Hiding a popped-out section: bring it home first, then hide it.
            self._popout_windows[section_key].close()
        self.settings.visible_sections[section_key] = visible
        self.overview_page.set_section_visible(section_key, visible)
        self.settings.save()

    def _on_style_updated(self, _widget):
        self._refresh_graph_palette()

    def _refresh_graph_palette(self):
        self.overview_page.refresh_palette(widget_uses_dark_theme(self))

    def _on_destroy(self, _window):
        width, height = self.get_size()
        self.settings.window_width = width
        self.settings.window_height = height
        self.settings.save()
        for popout in list(self._popout_windows.values()):
            popout.destroy()
        self.sampler.stop()
