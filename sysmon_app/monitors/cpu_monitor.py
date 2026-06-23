"""CPU monitoring backed by psutil."""

import os
from dataclasses import dataclass, field

import psutil


@dataclass
class CpuSnapshot:
    overall_percent: float = 0.0
    per_core_percent: list = field(default_factory=list)
    frequency_megahertz: float = None
    load_average_1m: float = 0.0
    load_average_5m: float = 0.0
    load_average_15m: float = 0.0
    core_count: int = 0


class CpuMonitor:
    def __init__(self):
        # Prime psutil's internal counters so the first real sample is accurate.
        psutil.cpu_percent(interval=None, percpu=True)

    def sample(self):
        snapshot = CpuSnapshot()
        snapshot.per_core_percent = psutil.cpu_percent(interval=None, percpu=True)
        snapshot.core_count = len(snapshot.per_core_percent)
        if snapshot.per_core_percent:
            snapshot.overall_percent = sum(snapshot.per_core_percent) / snapshot.core_count

        try:
            frequency = psutil.cpu_freq()
            if frequency is not None:
                snapshot.frequency_megahertz = frequency.current
        except (OSError, NotImplementedError):
            pass

        try:
            snapshot.load_average_1m, snapshot.load_average_5m, snapshot.load_average_15m = os.getloadavg()
        except OSError:
            pass

        return snapshot
