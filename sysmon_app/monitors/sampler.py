"""The background sampler thread.

Every interval it collects one ``SystemSnapshotBundle`` from all monitors and
delivers it to the UI on the GTK main loop via ``GLib.idle_add``. Sampling off
the main thread keeps the window responsive no matter how slow /proc is.
"""

import threading
import time
from dataclasses import dataclass, field

from gi.repository import GLib

from .cpu_monitor import CpuMonitor
from .disk_monitor import DiskMonitor
from .gpu_monitor import AmdGpuMonitor
from .memory_monitor import MemoryMonitor
from .network_monitor import NetworkMonitor
from .process_monitor import ProcessMonitor


@dataclass
class SystemSnapshotBundle:
    gpu: object = None  # GpuSnapshot or None when no AMD GPU exists
    cpu: object = None
    memory: object = None
    network: object = None
    disks: list = field(default_factory=list)
    processes: dict = field(default_factory=dict)  # pid -> ProcessRecord

    def top_processes_by(self, sort_key, count=3, minimum_value=0.0):
        """The busiest processes by an attribute, e.g. 'resident_memory_bytes'."""
        candidates = [
            record
            for record in self.processes.values()
            if (getattr(record, sort_key) or 0) > minimum_value
        ]
        candidates.sort(key=lambda record: getattr(record, sort_key) or 0, reverse=True)
        return candidates[:count]


class SystemSamplerThread(threading.Thread):
    """Calls ``on_snapshot_ready(bundle)`` on the GTK main loop each interval."""

    def __init__(self, on_snapshot_ready, update_interval_seconds=2.0):
        super().__init__(name="system-sampler", daemon=True)
        self._on_snapshot_ready = on_snapshot_ready
        self._update_interval_seconds = update_interval_seconds
        self._stop_event = threading.Event()
        self._paused = False

        self.gpu_monitor = AmdGpuMonitor()
        self.cpu_monitor = CpuMonitor()
        self.memory_monitor = MemoryMonitor()
        self.network_monitor = NetworkMonitor(update_interval_seconds)
        self.disk_monitor = DiskMonitor()
        self.process_monitor = ProcessMonitor()

    # -------------------------------------------------------------- controls

    def set_update_interval(self, seconds):
        self._update_interval_seconds = seconds

    def set_paused(self, paused):
        self._paused = paused

    def stop(self):
        self._stop_event.set()
        self.network_monitor.shutdown()

    # -------------------------------------------------------------- sampling

    def run(self):
        while not self._stop_event.is_set():
            cycle_started_at = time.monotonic()
            if not self._paused:
                bundle = self._collect_snapshot_bundle()
                GLib.idle_add(self._on_snapshot_ready, bundle, priority=GLib.PRIORITY_DEFAULT)
            elapsed = time.monotonic() - cycle_started_at
            self._stop_event.wait(max(0.1, self._update_interval_seconds - elapsed))

    def _collect_snapshot_bundle(self):
        bundle = SystemSnapshotBundle()
        # Each subsystem is sampled independently so one failure (a vanished
        # process, an unreadable sysfs file) never takes down the others.
        for collect in (
            self._collect_processes,  # first: others merge into process records
            self._collect_gpu,
            self._collect_cpu,
            self._collect_memory,
            self._collect_network,
            self._collect_disks,
        ):
            try:
                collect(bundle)
            except Exception:
                pass
        return bundle

    def _collect_processes(self, bundle):
        bundle.processes = self.process_monitor.sample()

    def _collect_gpu(self, bundle):
        bundle.gpu = self.gpu_monitor.sample()
        if bundle.gpu is None:
            return
        for gpu_usage in bundle.gpu.processes:
            record = bundle.processes.get(gpu_usage.pid)
            if record is not None:
                record.gpu_busy_percent = gpu_usage.busy_percent
                record.gpu_vram_bytes = gpu_usage.vram_bytes

    def _collect_cpu(self, bundle):
        bundle.cpu = self.cpu_monitor.sample()

    def _collect_memory(self, bundle):
        bundle.memory = self.memory_monitor.sample()

    def _collect_network(self, bundle):
        bundle.network = self.network_monitor.sample()
        per_process_rates = bundle.network.per_process_bytes_per_second
        if per_process_rates is None:
            return
        for pid, bytes_per_second in per_process_rates.items():
            record = bundle.processes.get(pid)
            if record is not None:
                record.network_bytes_per_second = bytes_per_second

    def _collect_disks(self, bundle):
        bundle.disks = self.disk_monitor.sample()
