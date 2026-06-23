"""Mounted-drive monitoring: read/write throughput per mounted filesystem.

Only real block devices are listed (no loop devices, tmpfs, or bind-mount
duplicates). Throughput comes from per-disk I/O counter deltas; a partition's
counters are matched by device name, falling back to its parent disk.
"""

import os
import time
from dataclasses import dataclass

import psutil


@dataclass
class MountedDriveActivity:
    device_path: str
    mount_point: str
    display_name: str  # filesystem label when available, otherwise device name
    read_bytes_per_second: float
    write_bytes_per_second: float
    used_bytes: int
    total_bytes: int


def _filesystem_labels_by_device():
    """Map real device paths to their filesystem labels via /dev/disk/by-label."""
    labels = {}
    by_label_directory = "/dev/disk/by-label"
    try:
        for label_name in os.listdir(by_label_directory):
            real_device = os.path.realpath(os.path.join(by_label_directory, label_name))
            # udev escapes special characters as \xNN sequences.
            readable_label = label_name.encode("ascii", "ignore").decode("unicode_escape")
            labels[real_device] = readable_label
    except OSError:
        pass
    return labels


def _counter_key_for_device(device_path, per_disk_counters):
    """Find the psutil disk-counter key for a device like /dev/nvme0n1p1."""
    real_device_name = os.path.basename(os.path.realpath(device_path))
    if real_device_name in per_disk_counters:
        return real_device_name
    # Fall back to the parent disk: nvme0n1p2 -> nvme0n1, sda1 -> sda.
    parent_disk_name = real_device_name.rstrip("0123456789")
    if parent_disk_name.endswith("p") and parent_disk_name[:-1] in per_disk_counters:
        return parent_disk_name[:-1]
    if parent_disk_name in per_disk_counters:
        return parent_disk_name
    return None


class DiskMonitor:
    def __init__(self):
        self._previous_counters = {}  # counter key -> (read_bytes, write_bytes)
        self._previous_sample_time = time.monotonic()

    def sample(self):
        try:
            per_disk_counters = psutil.disk_io_counters(perdisk=True)
        except (OSError, RuntimeError):
            per_disk_counters = {}
        current_time = time.monotonic()
        elapsed_seconds = max(current_time - self._previous_sample_time, 1e-6)
        labels_by_device = _filesystem_labels_by_device()

        drives = []
        seen_devices = set()
        for partition in psutil.disk_partitions(all=False):
            if not partition.device.startswith("/dev/"):
                continue
            if "/loop" in partition.device:
                continue
            real_device = os.path.realpath(partition.device)
            if real_device in seen_devices:
                continue  # bind mounts / btrfs subvolumes of the same device
            seen_devices.add(real_device)

            try:
                usage = psutil.disk_usage(partition.mountpoint)
            except OSError:
                continue

            read_rate = 0.0
            write_rate = 0.0
            counter_key = _counter_key_for_device(partition.device, per_disk_counters)
            if counter_key is not None:
                counters = per_disk_counters[counter_key]
                previous = self._previous_counters.get(counter_key)
                if previous is not None:
                    previous_read, previous_write = previous
                    read_rate = max(0.0, (counters.read_bytes - previous_read) / elapsed_seconds)
                    write_rate = max(0.0, (counters.write_bytes - previous_write) / elapsed_seconds)
                self._previous_counters[counter_key] = (counters.read_bytes, counters.write_bytes)

            drives.append(
                MountedDriveActivity(
                    device_path=partition.device,
                    mount_point=partition.mountpoint,
                    display_name=labels_by_device.get(real_device, os.path.basename(partition.device)),
                    read_bytes_per_second=read_rate,
                    write_bytes_per_second=write_rate,
                    used_bytes=usage.used,
                    total_bytes=usage.total,
                )
            )

        self._previous_sample_time = current_time
        drives.sort(key=lambda drive: drive.mount_point)
        return drives
