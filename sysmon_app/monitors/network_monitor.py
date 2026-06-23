"""Network monitoring.

System-wide up/down rates come from psutil interface counters (virtual
interfaces like lo/docker/veth are excluded).

Per-process bandwidth is fundamentally unavailable from /proc, so we lean on
``nethogs`` in trace mode when it is installed and allowed to capture packets
(root, or the binary carries cap_net_admin+cap_net_raw). When it is not
usable we expose a human-readable hint instead of silently showing nothing.
"""

import os
import shutil
import subprocess
import threading
import time
from dataclasses import dataclass, field

import psutil

_VIRTUAL_INTERFACE_PREFIXES = ("lo", "docker", "veth", "br-", "virbr", "vnet")


@dataclass
class NetworkSnapshot:
    download_bytes_per_second: float = 0.0
    upload_bytes_per_second: float = 0.0
    total_received_bytes: int = 0
    total_sent_bytes: int = 0
    # pid -> combined send+receive bytes/sec; None when no per-process source exists
    per_process_bytes_per_second: dict = None
    per_process_unavailable_hint: str = None


def _physical_interface_counters():
    received_total = 0
    sent_total = 0
    for interface_name, counters in psutil.net_io_counters(pernic=True).items():
        if interface_name.startswith(_VIRTUAL_INTERFACE_PREFIXES):
            continue
        received_total += counters.bytes_recv
        sent_total += counters.bytes_sent
    return received_total, sent_total


class NetworkMonitor:
    def __init__(self, update_interval_seconds=2.0):
        self._previous_counters = _physical_interface_counters()
        self._previous_sample_time = time.monotonic()
        self.per_process_sampler = NethogsPerProcessSampler(update_interval_seconds)

    def sample(self):
        snapshot = NetworkSnapshot()
        received_total, sent_total = _physical_interface_counters()
        current_time = time.monotonic()
        elapsed_seconds = max(current_time - self._previous_sample_time, 1e-6)

        previous_received, previous_sent = self._previous_counters
        snapshot.download_bytes_per_second = max(0.0, (received_total - previous_received) / elapsed_seconds)
        snapshot.upload_bytes_per_second = max(0.0, (sent_total - previous_sent) / elapsed_seconds)
        snapshot.total_received_bytes = received_total
        snapshot.total_sent_bytes = sent_total

        self._previous_counters = (received_total, sent_total)
        self._previous_sample_time = current_time

        if self.per_process_sampler.is_usable:
            snapshot.per_process_bytes_per_second = self.per_process_sampler.current_rates()
        else:
            snapshot.per_process_unavailable_hint = self.per_process_sampler.unavailable_hint
        return snapshot

    def shutdown(self):
        self.per_process_sampler.stop()


class NethogsPerProcessSampler:
    """Runs ``nethogs -t`` in the background and keeps the latest per-pid rates."""

    def __init__(self, update_interval_seconds):
        self._update_interval_seconds = update_interval_seconds
        self._latest_rates = {}
        self._rates_lock = threading.Lock()
        self._nethogs_process = None
        self._reader_thread = None
        self._stop_requested = False

        self.is_usable = False
        self.unavailable_hint = None
        self._determine_usability()
        if self.is_usable:
            self._start()

    def _determine_usability(self):
        nethogs_path = shutil.which("nethogs")
        if nethogs_path is None:
            self.unavailable_hint = (
                "Per-process network needs nethogs:  sudo apt install nethogs\n"
                "then:  sudo setcap 'cap_net_admin,cap_net_raw+ep' $(which nethogs)"
            )
            return
        try:
            getcap_output = subprocess.run(
                ["getcap", nethogs_path], capture_output=True, text=True, timeout=5
            ).stdout
        except (OSError, subprocess.SubprocessError):
            getcap_output = ""
        has_capture_capabilities = "cap_net_admin" in getcap_output and "cap_net_raw" in getcap_output
        if os.geteuid() == 0 or has_capture_capabilities:
            self.is_usable = True
        else:
            self.unavailable_hint = (
                "nethogs is installed but cannot capture packets. Grant it permission with:\n"
                f"sudo setcap 'cap_net_admin,cap_net_raw+ep' {nethogs_path}"
            )

    def _start(self):
        refresh_seconds = max(1, int(round(self._update_interval_seconds)))
        try:
            self._nethogs_process = subprocess.Popen(
                ["nethogs", "-t", "-d", str(refresh_seconds)],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
            )
        except OSError:
            self.is_usable = False
            self.unavailable_hint = "nethogs failed to start."
            return
        self._reader_thread = threading.Thread(
            target=self._read_trace_output, name="nethogs-reader", daemon=True
        )
        self._reader_thread.start()

    def _read_trace_output(self):
        """Trace mode prints a 'Refreshing:' marker followed by one line per
        process: <path>/<pid>/<uid> TAB <sent kB/s> TAB <received kB/s>."""
        rates_being_collected = {}
        try:
            for line in self._nethogs_process.stdout:
                if self._stop_requested:
                    break
                line = line.rstrip("\n")
                if line.startswith("Refreshing"):
                    with self._rates_lock:
                        self._latest_rates = rates_being_collected
                    rates_being_collected = {}
                    continue
                parts = line.split("\t")
                if len(parts) < 3:
                    continue
                identity = parts[0]
                try:
                    sent_kilobytes = float(parts[-2])
                    received_kilobytes = float(parts[-1])
                    pid = int(identity.rsplit("/", 2)[-2])
                except (ValueError, IndexError):
                    continue
                if pid <= 0:
                    continue  # nethogs reports unattributed traffic as pid 0
                combined_bytes_per_second = (sent_kilobytes + received_kilobytes) * 1024.0
                rates_being_collected[pid] = (
                    rates_being_collected.get(pid, 0.0) + combined_bytes_per_second
                )
        except (OSError, ValueError):
            pass

    def current_rates(self):
        with self._rates_lock:
            return dict(self._latest_rates)

    def stop(self):
        self._stop_requested = True
        if self._nethogs_process is not None:
            try:
                self._nethogs_process.terminate()
            except OSError:
                pass
