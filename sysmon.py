#!/usr/bin/env python3
"""SysMon — a compact, theme-aware system monitor for Cinnamon.

Launch directly:  python3 sysmon.py
"""

import ctypes
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def adopt_process_name():
    """Show up as 'sysmon' in process lists instead of 'python3'."""
    PR_SET_NAME = 15
    try:
        libc = ctypes.CDLL("libc.so.6", use_errno=True)
        libc.prctl(PR_SET_NAME, b"sysmon", 0, 0, 0)
    except OSError:
        pass


adopt_process_name()

from gi.repository import GLib

GLib.set_prgname("sysmon")  # also gives the window the right WM_CLASS

from sysmon_app.application import SysMonApplication

if __name__ == "__main__":
    sys.exit(SysMonApplication().run(sys.argv))
