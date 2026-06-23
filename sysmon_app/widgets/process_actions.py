"""Process control actions (end / kill / renice) and the shared context menu.

Actions are attempted directly first; when permissions are insufficient (a
root-owned process, or raising priority) we fall back to ``pkexec`` so the
user gets the normal system authentication dialog.
"""

import subprocess
import threading

import gi
import psutil

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk

PRIORITY_PRESETS = [
    ("Very High", -12),
    ("High", -5),
    ("Normal", 0),
    ("Low", 10),
    ("Very Low", 19),
]


def _run_privileged_fallback(command_arguments, parent_window, failure_message):
    """Run a pkexec command without blocking the UI; report failure if it fails."""

    def run_and_report():
        try:
            completed = subprocess.run(command_arguments, capture_output=True, timeout=120)
            # 126/127 mean the auth dialog was dismissed — not an error worth nagging about.
            if completed.returncode not in (0, 126, 127):
                GLib.idle_add(_show_error_dialog, parent_window, failure_message)
        except (OSError, subprocess.SubprocessError):
            GLib.idle_add(_show_error_dialog, parent_window, failure_message)

    threading.Thread(target=run_and_report, daemon=True).start()


def _show_error_dialog(parent_window, message):
    dialog = Gtk.MessageDialog(
        transient_for=parent_window,
        modal=True,
        message_type=Gtk.MessageType.ERROR,
        buttons=Gtk.ButtonsType.OK,
        text="Process action failed",
    )
    dialog.format_secondary_text(message)
    dialog.run()
    dialog.destroy()
    return False


def _confirm_action(parent_window, title, detail):
    dialog = Gtk.MessageDialog(
        transient_for=parent_window,
        modal=True,
        message_type=Gtk.MessageType.QUESTION,
        buttons=Gtk.ButtonsType.NONE,
        text=title,
    )
    dialog.format_secondary_text(detail)
    dialog.add_button("Cancel", Gtk.ResponseType.CANCEL)
    confirm_button = dialog.add_button(title.split(" ")[0], Gtk.ResponseType.OK)
    confirm_button.get_style_context().add_class("destructive-action")
    response = dialog.run()
    dialog.destroy()
    return response == Gtk.ResponseType.OK


def end_process(pid, process_name, parent_window):
    if not _confirm_action(
        parent_window,
        f"End “{process_name}”?",
        f"PID {pid} will be asked to exit (SIGTERM). Unsaved work may be lost.",
    ):
        return
    try:
        psutil.Process(pid).terminate()
    except psutil.NoSuchProcess:
        pass
    except (psutil.AccessDenied, PermissionError):
        _run_privileged_fallback(
            ["pkexec", "kill", "-TERM", str(pid)], parent_window,
            f"Could not end {process_name} (PID {pid}).",
        )


def kill_process(pid, process_name, parent_window):
    if not _confirm_action(
        parent_window,
        f"Kill “{process_name}”?",
        f"PID {pid} will be terminated immediately (SIGKILL). Unsaved work will be lost.",
    ):
        return
    try:
        psutil.Process(pid).kill()
    except psutil.NoSuchProcess:
        pass
    except (psutil.AccessDenied, PermissionError):
        _run_privileged_fallback(
            ["pkexec", "kill", "-KILL", str(pid)], parent_window,
            f"Could not kill {process_name} (PID {pid}).",
        )


def set_process_priority(pid, process_name, nice_value, parent_window):
    try:
        psutil.Process(pid).nice(nice_value)
    except psutil.NoSuchProcess:
        pass
    except (psutil.AccessDenied, PermissionError):
        # Raising priority (or touching another user's process) needs root.
        _run_privileged_fallback(
            ["pkexec", "renice", "-n", str(nice_value), "-p", str(pid)], parent_window,
            f"Could not change the priority of {process_name} (PID {pid}).",
        )


def build_process_context_menu(pid, process_name, parent_window, current_nice_value=None):
    """A ready-to-pop Gtk.Menu with end/kill/priority/copy actions."""
    menu = Gtk.Menu()

    header_item = Gtk.MenuItem(label=f"{process_name}  (PID {pid})")
    header_item.set_sensitive(False)
    menu.append(header_item)
    menu.append(Gtk.SeparatorMenuItem())

    end_item = Gtk.MenuItem(label="End Process")
    end_item.connect("activate", lambda _item: end_process(pid, process_name, parent_window))
    menu.append(end_item)

    kill_item = Gtk.MenuItem(label="Force Kill")
    kill_item.connect("activate", lambda _item: kill_process(pid, process_name, parent_window))
    menu.append(kill_item)

    menu.append(Gtk.SeparatorMenuItem())

    priority_submenu = Gtk.Menu()
    for preset_label, nice_value in PRIORITY_PRESETS:
        is_current = current_nice_value is not None and nice_value == current_nice_value
        item_label = f"{preset_label} ({nice_value:+d})" + ("  ✓" if is_current else "")
        priority_item = Gtk.MenuItem(label=item_label)
        priority_item.connect(
            "activate",
            lambda _item, value=nice_value: set_process_priority(
                pid, process_name, value, parent_window
            ),
        )
        priority_submenu.append(priority_item)
    priority_root_item = Gtk.MenuItem(label="Set Priority")
    priority_root_item.set_submenu(priority_submenu)
    menu.append(priority_root_item)

    menu.append(Gtk.SeparatorMenuItem())

    copy_pid_item = Gtk.MenuItem(label="Copy PID")
    copy_pid_item.connect("activate", lambda _item: _copy_to_clipboard(str(pid)))
    menu.append(copy_pid_item)

    menu.show_all()
    return menu


def _copy_to_clipboard(text):
    from gi.repository import Gdk

    clipboard = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
    clipboard.set_text(text, -1)
