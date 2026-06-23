"""Human-readable formatting helpers shared across the UI.

Sizes and rates follow the user's units preference:

  * decimal (default) — GB, MB/s: the numbers your drive sticker and ISP quote
  * binary            — GiB, MiB/s: the powers-of-two numbers htop and
                        GNOME System Monitor show

Switch with ``set_binary_units``; every label picks the change up on the next
sampler tick because formatting happens at render time.
"""

_DECIMAL_SIZE_UNITS = ["B", "kB", "MB", "GB", "TB", "PB"]
_DECIMAL_RATE_UNITS = ["B/s", "kB/s", "MB/s", "GB/s", "TB/s"]
_BINARY_SIZE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"]
_BINARY_RATE_UNITS = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"]

_use_binary_units = False


def set_binary_units(enabled):
    """Choose binary (GiB/MiB, base 1024) or decimal (GB/MB, base 1000)."""
    global _use_binary_units
    _use_binary_units = bool(enabled)


def _format_scaled(raw_value, unit_names, unit_step):
    value = float(raw_value)
    for unit in unit_names:
        if value < unit_step or unit == unit_names[-1]:
            if unit == unit_names[0]:
                return f"{int(value)} {unit}"
            return f"{value:.1f} {unit}"
        value /= unit_step
    return f"{value:.1f} {unit_names[-1]}"


def format_size(size_in_bytes):
    """Format a byte count as a size, e.g. '9.8 GB' or '9.1 GiB'."""
    if size_in_bytes is None:
        return "—"
    if _use_binary_units:
        return _format_scaled(size_in_bytes, _BINARY_SIZE_UNITS, 1024.0)
    return _format_scaled(size_in_bytes, _DECIMAL_SIZE_UNITS, 1000.0)


def format_rate(bytes_per_second):
    """Format a throughput as a rate, e.g. '12.3 MB/s' or '11.7 MiB/s'."""
    if bytes_per_second is None:
        return "—"
    if _use_binary_units:
        return _format_scaled(bytes_per_second, _BINARY_RATE_UNITS, 1024.0)
    return _format_scaled(bytes_per_second, _DECIMAL_RATE_UNITS, 1000.0)


def format_percent(percent_value):
    if percent_value is None:
        return "—"
    return f"{percent_value:.0f}%"


def format_frequency_from_megahertz(megahertz):
    if megahertz is None:
        return "—"
    if megahertz >= 1000.0:
        return f"{megahertz / 1000.0:.2f} GHz"
    return f"{megahertz:.0f} MHz"


def format_temperature_celsius(degrees_celsius):
    if degrees_celsius is None:
        return "—"
    return f"{degrees_celsius:.0f}°C"


def format_power_watts(watts):
    if watts is None:
        return "—"
    return f"{watts:.0f} W"
