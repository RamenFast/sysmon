"""System RAM and swap monitoring backed by psutil."""

from dataclasses import dataclass

import psutil


@dataclass
class MemorySnapshot:
    total_bytes: int = 0
    used_bytes: int = 0
    available_bytes: int = 0
    cached_bytes: int = 0
    used_percent: float = 0.0
    swap_total_bytes: int = 0
    swap_used_bytes: int = 0


class MemoryMonitor:
    def sample(self):
        virtual_memory = psutil.virtual_memory()
        swap_memory = psutil.swap_memory()
        return MemorySnapshot(
            total_bytes=virtual_memory.total,
            used_bytes=virtual_memory.total - virtual_memory.available,
            available_bytes=virtual_memory.available,
            cached_bytes=getattr(virtual_memory, "cached", 0),
            used_percent=virtual_memory.percent,
            swap_total_bytes=swap_memory.total,
            swap_used_bytes=swap_memory.used,
        )
