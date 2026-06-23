"""Per-process monitoring: CPU, memory, and disk I/O for every process.

GPU and network figures are merged in afterwards by the sampler, since they
come from the GPU monitor (fdinfo) and nethogs respectively.
"""

import time
from dataclasses import dataclass

import psutil

_PROCESS_ATTRIBUTES = [
    "pid",
    "name",
    "username",
    "cpu_percent",
    "memory_info",
    "nice",
    "status",
    "cmdline",
    "io_counters",
]


@dataclass
class ProcessRecord:
    pid: int
    name: str
    username: str
    cpu_percent: float
    resident_memory_bytes: int
    nice_value: int
    status: str
    command_line: str
    disk_read_bytes_per_second: float = 0.0
    disk_write_bytes_per_second: float = 0.0
    # Merged in by the sampler:
    gpu_busy_percent: float = 0.0
    gpu_vram_bytes: int = 0
    network_bytes_per_second: float = None


class ProcessMonitor:
    def __init__(self):
        self._previous_io_counters = {}  # pid -> (read_bytes, write_bytes)
        self._previous_sample_time = time.monotonic()
        # Prime psutil's per-process CPU counters.
        for process in psutil.process_iter(["cpu_percent"]):
            pass

    def sample(self):
        current_time = time.monotonic()
        elapsed_seconds = max(current_time - self._previous_sample_time, 1e-6)
        records = {}
        current_io_counters = {}

        for process in psutil.process_iter(_PROCESS_ATTRIBUTES):
            info = process.info
            pid = info["pid"]
            memory_info = info.get("memory_info")
            command_line_parts = info.get("cmdline") or []

            record = ProcessRecord(
                pid=pid,
                name=info.get("name") or f"pid {pid}",
                username=info.get("username") or "?",
                cpu_percent=info.get("cpu_percent") or 0.0,
                resident_memory_bytes=memory_info.rss if memory_info else 0,
                nice_value=info.get("nice") if info.get("nice") is not None else 0,
                status=info.get("status") or "?",
                command_line=" ".join(command_line_parts),
            )

            io_counters = info.get("io_counters")
            if io_counters is not None:
                current_io_counters[pid] = (io_counters.read_bytes, io_counters.write_bytes)
                previous = self._previous_io_counters.get(pid)
                if previous is not None:
                    previous_read, previous_write = previous
                    record.disk_read_bytes_per_second = max(
                        0.0, (io_counters.read_bytes - previous_read) / elapsed_seconds
                    )
                    record.disk_write_bytes_per_second = max(
                        0.0, (io_counters.write_bytes - previous_write) / elapsed_seconds
                    )

            records[pid] = record

        self._previous_io_counters = current_io_counters
        self._previous_sample_time = current_time
        return records
