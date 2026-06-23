"""AMD GPU monitoring via the amdgpu driver's sysfs interface.

Card-level metrics come from /sys/class/drm/cardN/device (busy percent, VRAM)
and its hwmon directory (temperatures, power, clocks, fan).

Per-process usage comes from /proc/<pid>/fdinfo: any process holding a DRM
file descriptor reports cumulative engine time (drm-engine-gfx/compute/...)
and resident VRAM (drm-memory-vram). Engine-time deltas between samples give
a utilisation percentage, exactly the way nvtop and amdgpu_top compute it.
"""

import os
import re
import stat
import subprocess
import time
from dataclasses import dataclass, field

AMD_PCI_VENDOR_ID = "0x1002"
DRM_CHARACTER_DEVICE_MAJOR = 226

# Engine-time keys we treat as "the GPU is busy". A process's utilisation is
# the maximum across engines so graphics and compute workloads both register
# without double-counting.
_ENGINE_TIME_KEYS = (
    "drm-engine-gfx",
    "drm-engine-compute",
    "drm-engine-enc",
    "drm-engine-dec",
)

# Processes without DRM file descriptors are re-checked only every N samples
# to keep the /proc scan cheap.
_NEGATIVE_CACHE_SAMPLE_COUNT = 5


@dataclass
class GpuProcessUsage:
    pid: int
    busy_percent: float
    vram_bytes: int


@dataclass
class GpuSnapshot:
    device_name: str = "GPU"
    busy_percent: float = 0.0
    vram_used_bytes: int = 0
    vram_total_bytes: int = 0
    temperature_edge_celsius: float = None
    temperature_junction_celsius: float = None
    power_draw_watts: float = None
    power_cap_watts: float = None
    core_clock_megahertz: float = None
    memory_clock_megahertz: float = None
    fan_speed_rpm: int = None
    processes: list = field(default_factory=list)


def _read_sysfs_value(file_path):
    try:
        with open(file_path, "r", encoding="ascii") as sysfs_file:
            return sysfs_file.read().strip()
    except (OSError, ValueError):
        return None


def _read_sysfs_integer(file_path):
    raw_value = _read_sysfs_value(file_path)
    if raw_value is None or raw_value == "":
        return None
    try:
        return int(raw_value)
    except ValueError:
        return None


class AmdGpuMonitor:
    """Samples one amdgpu card. ``is_available`` is False on systems without one."""

    def __init__(self):
        self._device_path = None
        self._hwmon_path = None
        self._pci_address = None
        self._device_name = "GPU"

        self._previous_engine_time_by_pid = {}  # pid -> (engine_ns, monotonic_time)
        self._pids_without_drm_files = {}  # pid -> samples remaining before recheck

        self._discover_card()
        if self.is_available:
            self._device_name = self._query_marketing_name()

    @property
    def is_available(self):
        return self._device_path is not None

    def _discover_card(self):
        try:
            card_entries = sorted(os.listdir("/sys/class/drm"))
        except OSError:
            return
        for entry_name in card_entries:
            if not re.fullmatch(r"card\d+", entry_name):
                continue
            device_path = f"/sys/class/drm/{entry_name}/device"
            vendor = _read_sysfs_value(os.path.join(device_path, "vendor"))
            has_busy_metric = os.path.exists(os.path.join(device_path, "gpu_busy_percent"))
            if vendor == AMD_PCI_VENDOR_ID and has_busy_metric:
                self._device_path = device_path
                self._pci_address = os.path.basename(os.path.realpath(device_path))
                self._hwmon_path = self._find_hwmon_directory(device_path)
                return

    @staticmethod
    def _find_hwmon_directory(device_path):
        hwmon_parent = os.path.join(device_path, "hwmon")
        try:
            for entry_name in sorted(os.listdir(hwmon_parent)):
                if entry_name.startswith("hwmon"):
                    return os.path.join(hwmon_parent, entry_name)
        except OSError:
            pass
        return None

    def _query_marketing_name(self):
        """Ask lspci for a human-readable card name; fall back to a generic one."""
        try:
            pci_slot = self._pci_address.split(":", 1)[1]  # drop the "0000:" domain
            lspci_output = subprocess.run(
                ["lspci", "-mm", "-s", pci_slot],
                capture_output=True, text=True, timeout=5,
            ).stdout
            quoted_fields = re.findall(r'"([^"]*)"', lspci_output)
            if len(quoted_fields) >= 3:
                device_field = quoted_fields[2]
                bracketed_name = re.search(r"\[([^\]]+)\]", device_field)
                return bracketed_name.group(1) if bracketed_name else device_field
        except (OSError, subprocess.SubprocessError, IndexError):
            pass
        return "AMD GPU"

    # ---------------------------------------------------------------- sampling

    def sample(self):
        if not self.is_available:
            return None

        snapshot = GpuSnapshot(device_name=self._device_name)
        device = self._device_path
        hwmon = self._hwmon_path

        busy = _read_sysfs_integer(os.path.join(device, "gpu_busy_percent"))
        snapshot.busy_percent = float(busy) if busy is not None else 0.0
        snapshot.vram_used_bytes = _read_sysfs_integer(os.path.join(device, "mem_info_vram_used")) or 0
        snapshot.vram_total_bytes = _read_sysfs_integer(os.path.join(device, "mem_info_vram_total")) or 0

        if hwmon:
            edge_millicelsius = _read_sysfs_integer(os.path.join(hwmon, "temp1_input"))
            junction_millicelsius = _read_sysfs_integer(os.path.join(hwmon, "temp2_input"))
            if edge_millicelsius is not None:
                snapshot.temperature_edge_celsius = edge_millicelsius / 1000.0
            if junction_millicelsius is not None:
                snapshot.temperature_junction_celsius = junction_millicelsius / 1000.0

            power_microwatts = _read_sysfs_integer(
                os.path.join(hwmon, "power1_average")
            ) or _read_sysfs_integer(os.path.join(hwmon, "power1_input"))
            if power_microwatts is not None:
                snapshot.power_draw_watts = power_microwatts / 1_000_000.0
            power_cap_microwatts = _read_sysfs_integer(os.path.join(hwmon, "power1_cap"))
            if power_cap_microwatts is not None:
                snapshot.power_cap_watts = power_cap_microwatts / 1_000_000.0

            core_clock_hertz = _read_sysfs_integer(os.path.join(hwmon, "freq1_input"))
            memory_clock_hertz = _read_sysfs_integer(os.path.join(hwmon, "freq2_input"))
            if core_clock_hertz is not None:
                snapshot.core_clock_megahertz = core_clock_hertz / 1_000_000.0
            if memory_clock_hertz is not None:
                snapshot.memory_clock_megahertz = memory_clock_hertz / 1_000_000.0

            snapshot.fan_speed_rpm = _read_sysfs_integer(os.path.join(hwmon, "fan1_input"))

        snapshot.processes = self._sample_per_process_usage()
        return snapshot

    # ------------------------------------------------- per-process accounting

    def _sample_per_process_usage(self):
        usage_records = []
        seen_drm_client_ids = set()
        current_time = time.monotonic()
        live_pids = set()

        try:
            proc_entries = os.listdir("/proc")
        except OSError:
            return usage_records

        for entry_name in proc_entries:
            if not entry_name.isdigit():
                continue
            pid = int(entry_name)
            live_pids.add(pid)

            remaining = self._pids_without_drm_files.get(pid)
            if remaining is not None and remaining > 0:
                self._pids_without_drm_files[pid] = remaining - 1
                continue

            engine_time_ns, vram_bytes = self._read_process_drm_usage(pid, seen_drm_client_ids)
            if engine_time_ns is None:
                self._pids_without_drm_files[pid] = _NEGATIVE_CACHE_SAMPLE_COUNT
                self._previous_engine_time_by_pid.pop(pid, None)
                continue
            self._pids_without_drm_files.pop(pid, None)

            busy_percent = 0.0
            previous = self._previous_engine_time_by_pid.get(pid)
            if previous is not None:
                previous_engine_ns, previous_time = previous
                elapsed_seconds = current_time - previous_time
                if elapsed_seconds > 0:
                    busy_percent = (engine_time_ns - previous_engine_ns) / (elapsed_seconds * 1e9) * 100.0
                    busy_percent = max(0.0, min(100.0, busy_percent))
            self._previous_engine_time_by_pid[pid] = (engine_time_ns, current_time)

            if busy_percent > 0.05 or vram_bytes > 0:
                usage_records.append(
                    GpuProcessUsage(pid=pid, busy_percent=busy_percent, vram_bytes=vram_bytes)
                )

        # Forget processes that have exited.
        self._previous_engine_time_by_pid = {
            pid: value for pid, value in self._previous_engine_time_by_pid.items() if pid in live_pids
        }
        self._pids_without_drm_files = {
            pid: value for pid, value in self._pids_without_drm_files.items() if pid in live_pids
        }

        usage_records.sort(key=lambda record: (record.busy_percent, record.vram_bytes), reverse=True)
        return usage_records

    def _read_process_drm_usage(self, pid, seen_drm_client_ids):
        """Return (max engine time in ns, VRAM bytes) summed over the process's
        DRM clients on this card, or (None, 0) if it holds no DRM files."""
        fd_directory = f"/proc/{pid}/fd"
        try:
            fd_names = os.listdir(fd_directory)
        except OSError:
            return None, 0

        total_engine_time_ns = 0
        total_vram_bytes = 0
        found_drm_file = False

        for fd_name in fd_names:
            try:
                fd_stat = os.stat(os.path.join(fd_directory, fd_name))
            except OSError:
                continue
            if not stat.S_ISCHR(fd_stat.st_mode):
                continue
            if os.major(fd_stat.st_rdev) != DRM_CHARACTER_DEVICE_MAJOR:
                continue
            found_drm_file = True

            fields = self._parse_fdinfo(f"/proc/{pid}/fdinfo/{fd_name}")
            if fields is None:
                continue
            if fields.get("drm-pdev") != self._pci_address:
                continue
            client_id = fields.get("drm-client-id")
            if client_id is None or client_id in seen_drm_client_ids:
                continue  # duplicate fd for the same client (dup/fork)
            seen_drm_client_ids.add(client_id)

            engine_times = [
                fields[key] for key in _ENGINE_TIME_KEYS if isinstance(fields.get(key), int)
            ]
            if engine_times:
                total_engine_time_ns += max(engine_times)
            vram_kibibytes = fields.get("drm-memory-vram") or fields.get("drm-total-vram") or 0
            total_vram_bytes += vram_kibibytes * 1024

        if not found_drm_file:
            return None, 0
        return total_engine_time_ns, total_vram_bytes

    @staticmethod
    def _parse_fdinfo(fdinfo_path):
        """Parse fdinfo into {key: value}; numeric values become integers and
        'NNN KiB' / 'NNN ns' suffixes are stripped."""
        try:
            with open(fdinfo_path, "r", encoding="ascii", errors="replace") as fdinfo_file:
                content = fdinfo_file.read()
        except OSError:
            return None

        fields = {}
        for line in content.splitlines():
            key, separator, raw_value = line.partition(":")
            if not separator:
                continue
            value = raw_value.strip()
            first_token = value.split(" ", 1)[0] if value else ""
            if first_token.lstrip("-").isdigit():
                fields[key] = int(first_token)
            else:
                fields[key] = value
        return fields
