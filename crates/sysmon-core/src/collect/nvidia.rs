// SPDX-License-Identifier: GPL-3.0-or-later
//! NVIDIA GPUs through NVML, the library nvidia-smi and nvtop read
//! (`libnvidia-ml.so.1`, shipped with the proprietary driver). It is
//! opened at run time with dlopen, so SysMon builds and runs without
//! it: no library, no driver or no card all read `available: false`.
//!
//! NVML is the one source. The proprietary driver keeps no
//! amdgpu-style sysfs counters, and a compute-only card (nvidia-drm
//! not loaded, as on Ben's RTX 3070 beside the R9700) has no DRM node,
//! so fdinfo has nothing to say about it either.
//!
//! Every reading is NVML's own, named for what it is:
//! - busy: `nvmlDeviceGetUtilizationRates().gpu`, the share of the
//!   driver's last sample period a kernel ran (nvidia-smi's
//!   `utilization.gpu`, same call);
//! - memory: `nvmlMemory_v2`, whose `used` leaves out the driver's
//!   own reserve. nvidia-smi prints the reserve as its own row;
//!   `nvmlMemory_v1` folds it into used. A driver without v2 gets v1,
//!   reserve included, and no reserve figure;
//! - the die sensor, the power draw (a 1 s average on Ampere and
//!   newer), the enforced power limit, the current graphics and memory
//!   clocks, the fan's target speed (and its RPM where the driver
//!   reports it);
//! - per process: memory from the compute and graphics process lists;
//!   busy is the driver's latest sample for each process newer than
//!   the previous collect (NVML keeps one per process that did work).
//!
//! The collector reads the first NVIDIA card (NVML index 0).

use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_int, c_uint, c_ulonglong, c_void};

use crate::snapshot::{GpuProcessUsage, GpuSnapshot};

type Return = c_int;
type Device = *mut c_void;

const SUCCESS: Return = 0;
const ERROR_INSUFFICIENT_SIZE: Return = 7;
/// `NVML_VALUE_NOT_AVAILABLE` (-1) as an unsigned long long.
const VALUE_NOT_AVAILABLE: c_ulonglong = c_ulonglong::MAX;
/// `NVML_TEMPERATURE_GPU`: the die sensor.
const TEMPERATURE_GPU: c_int = 0;
const CLOCK_GRAPHICS: c_int = 0;
const CLOCK_MEM: c_int = 2;
/// `NVML_DEVICE_NAME_V2_BUFFER_SIZE`.
const NAME_BUFFER: usize = 96;
/// `NVML_SYSTEM_DRIVER_VERSION_BUFFER_SIZE`.
const DRIVER_VERSION_BUFFER: usize = 80;
/// Process slots asked for per list; NVML says when it needs more.
const PROCESS_SLOTS: usize = 64;

// The structs below copy nvml.h (CUDA 13.4.3) field for field; the
// sizes are pinned in the tests against the header's layout.

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct MemoryV1 {
    total: c_ulonglong,
    free: c_ulonglong,
    /// Reserved + allocated.
    used: c_ulonglong,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct MemoryV2 {
    version: c_uint,
    total: c_ulonglong,
    reserved: c_ulonglong,
    free: c_ulonglong,
    /// Allocated only.
    used: c_ulonglong,
}

/// `nvmlProcessInfo_t` (= `nvmlProcessInfo_v2_t`), what the `_v3`
/// process-list calls fill.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct ProcessInfo {
    pid: c_uint,
    used_gpu_memory: c_ulonglong,
    gpu_instance_id: c_uint,
    compute_instance_id: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct ProcessUtilizationSample {
    pid: c_uint,
    /// CPU timestamp, microseconds.
    time_stamp: c_ulonglong,
    sm_util: c_uint,
    mem_util: c_uint,
    enc_util: c_uint,
    dec_util: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct FanSpeedInfo {
    version: c_uint,
    fan: c_uint,
    speed: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct PciInfo {
    bus_id_legacy: [c_char; 16],
    domain: c_uint,
    bus: c_uint,
    device: c_uint,
    /// Device id in the high 16 bits, vendor id in the low 16.
    pci_device_id: c_uint,
    pci_sub_system_id: c_uint,
    bus_id: [c_char; 32],
}

/// `NVML_STRUCT_VERSION(data, ver)`: the struct's size, the version in
/// the top byte.
const fn struct_version(size: usize, version: u32) -> c_uint {
    size as c_uint | (version << 24)
}
const MEMORY_V2: c_uint = struct_version(size_of::<MemoryV2>(), 2);
const FAN_SPEED_INFO_V1: c_uint = struct_version(size_of::<FanSpeedInfo>(), 1);

type DeviceU32 = unsafe extern "C" fn(Device, *mut c_uint) -> Return;
type DeviceEnumU32 = unsafe extern "C" fn(Device, c_int, *mut c_uint) -> Return;
type ProcessList = unsafe extern "C" fn(Device, *mut c_uint, *mut ProcessInfo) -> Return;

/// The entry points SysMon calls. The first block must exist (any
/// NVML since 2016 has them); the rest are newer than some drivers
/// and are simply skipped where absent.
struct Library {
    shutdown: unsafe extern "C" fn() -> Return,
    get_name: unsafe extern "C" fn(Device, *mut c_char, c_uint) -> Return,
    get_utilization: unsafe extern "C" fn(Device, *mut Utilization) -> Return,
    get_memory_v1: unsafe extern "C" fn(Device, *mut MemoryV1) -> Return,

    get_memory_v2: Option<unsafe extern "C" fn(Device, *mut MemoryV2) -> Return>,
    get_temperature: Option<DeviceEnumU32>,
    get_power_usage: Option<DeviceU32>,
    get_power_limit: Option<DeviceU32>,
    get_power_constraints: Option<unsafe extern "C" fn(Device, *mut c_uint, *mut c_uint) -> Return>,
    get_clock: Option<DeviceEnumU32>,
    get_fan_speed: Option<DeviceU32>,
    get_fan_speed_rpm: Option<unsafe extern "C" fn(Device, *mut FanSpeedInfo) -> Return>,
    get_compute_processes: Option<ProcessList>,
    get_graphics_processes: Option<ProcessList>,
    get_process_utilization:
        Option<unsafe extern "C" fn(Device, *mut ProcessUtilizationSample, *mut c_uint, c_ulonglong) -> Return>,
    get_pci_info: Option<unsafe extern "C" fn(Device, *mut PciInfo) -> Return>,
    get_architecture: Option<DeviceU32>,
    get_num_cores: Option<DeviceU32>,
    get_compute_capability: Option<unsafe extern "C" fn(Device, *mut c_int, *mut c_int) -> Return>,
    get_max_link_generation: Option<DeviceU32>,
    get_driver_version: Option<unsafe extern "C" fn(*mut c_char, c_uint) -> Return>,
    get_cuda_version: Option<unsafe extern "C" fn(*mut c_int) -> Return>,
}

/// One symbol from an open library, as the function-pointer type `F`.
///
/// # Safety
/// `F` must be the C signature the library exports under `name`.
unsafe fn symbol<F: Copy>(handle: *mut c_void, name: &CStr) -> Option<F> {
    debug_assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
    let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
    // SAFETY: a non-null dlsym result is the function's address; the
    // caller names its signature.
    (!address.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&address) })
}

/// An initialized NVML and its first device. Dropping it shuts NVML
/// down (NVML counts inits, so a second session elsewhere, such as
/// the spec sheet's, is unaffected).
struct Session {
    library: Library,
    device: Device,
}

// SAFETY: NVML documents its API as thread-safe, and a device handle
// is a process-wide identifier that stays valid until nvmlShutdown,
// which only Drop calls. The session moves between threads with its
// owner (the sampler) and is used through &self / &mut self only.
unsafe impl Send for Session {}

impl Session {
    /// dlopen, every symbol, nvmlInit_v2, device 0. None on any miss.
    fn open() -> Option<Session> {
        // SAFETY: the library name is a valid C string; RTLD_LOCAL
        // keeps NVML's symbols out of the global namespace.
        let handle = unsafe { libc::dlopen(c"libnvidia-ml.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return None;
        }
        // The handle is never closed: NVML stays mapped for the
        // process's life (dlopen counts references, so a second open
        // returns the same mapping).
        unsafe {
            let init: unsafe extern "C" fn() -> Return = symbol(handle, c"nvmlInit_v2")?;
            let library = Library {
                shutdown: symbol(handle, c"nvmlShutdown")?,
                get_name: symbol(handle, c"nvmlDeviceGetName")?,
                get_utilization: symbol(handle, c"nvmlDeviceGetUtilizationRates")?,
                get_memory_v1: symbol(handle, c"nvmlDeviceGetMemoryInfo")?,
                get_memory_v2: symbol(handle, c"nvmlDeviceGetMemoryInfo_v2"),
                get_temperature: symbol(handle, c"nvmlDeviceGetTemperature"),
                get_power_usage: symbol(handle, c"nvmlDeviceGetPowerUsage"),
                get_power_limit: symbol(handle, c"nvmlDeviceGetEnforcedPowerLimit"),
                get_power_constraints: symbol(handle, c"nvmlDeviceGetPowerManagementLimitConstraints"),
                get_clock: symbol(handle, c"nvmlDeviceGetClockInfo"),
                get_fan_speed: symbol(handle, c"nvmlDeviceGetFanSpeed"),
                get_fan_speed_rpm: symbol(handle, c"nvmlDeviceGetFanSpeedRPM"),
                get_compute_processes: symbol(handle, c"nvmlDeviceGetComputeRunningProcesses_v3"),
                get_graphics_processes: symbol(handle, c"nvmlDeviceGetGraphicsRunningProcesses_v3"),
                get_process_utilization: symbol(handle, c"nvmlDeviceGetProcessUtilization"),
                get_pci_info: symbol(handle, c"nvmlDeviceGetPciInfo_v3"),
                get_architecture: symbol(handle, c"nvmlDeviceGetArchitecture"),
                get_num_cores: symbol(handle, c"nvmlDeviceGetNumGpuCores"),
                get_compute_capability: symbol(handle, c"nvmlDeviceGetCudaComputeCapability"),
                get_max_link_generation: symbol(handle, c"nvmlDeviceGetMaxPcieLinkGeneration"),
                get_driver_version: symbol(handle, c"nvmlSystemGetDriverVersion"),
                get_cuda_version: symbol(handle, c"nvmlSystemGetCudaDriverVersion_v2"),
            };
            let get_count: unsafe extern "C" fn(*mut c_uint) -> Return = symbol(handle, c"nvmlDeviceGetCount_v2")?;
            let get_handle: unsafe extern "C" fn(c_uint, *mut Device) -> Return =
                symbol(handle, c"nvmlDeviceGetHandleByIndex_v2")?;
            if init() != SUCCESS {
                return None; // driver not loaded, or library/driver mismatch
            }
            // From here a failure must shut NVML down again.
            let mut count: c_uint = 0;
            let mut device: Device = std::ptr::null_mut();
            if get_count(&mut count) != SUCCESS || count == 0 || get_handle(0, &mut device) != SUCCESS {
                (library.shutdown)();
                return None;
            }
            Some(Session { library, device })
        }
    }

    fn name(&self) -> Option<String> {
        let mut buffer = [0 as c_char; NAME_BUFFER];
        // SAFETY: the buffer is NAME_BUFFER bytes and NVML writes a
        // NUL-terminated name no longer than that.
        let status = unsafe { (self.library.get_name)(self.device, buffer.as_mut_ptr(), NAME_BUFFER as c_uint) };
        (status == SUCCESS).then(|| c_string(&buffer)).filter(|name| !name.is_empty())
    }

    fn utilization(&self) -> Option<Utilization> {
        let mut out = Utilization::default();
        // SAFETY: out is a valid nvmlUtilization_t.
        (unsafe { (self.library.get_utilization)(self.device, &mut out) } == SUCCESS).then_some(out)
    }

    /// (used, total, reserved): v2 when the driver has it.
    fn memory(&self) -> Option<(u64, u64, Option<u64>)> {
        if let Some(get) = self.library.get_memory_v2 {
            let mut out = MemoryV2 { version: MEMORY_V2, ..Default::default() };
            // SAFETY: out is a valid nvmlMemory_v2_t with its version set.
            if unsafe { get(self.device, &mut out) } == SUCCESS {
                return Some((out.used, out.total, Some(out.reserved)));
            }
        }
        let mut out = MemoryV1::default();
        // SAFETY: out is a valid nvmlMemory_t.
        (unsafe { (self.library.get_memory_v1)(self.device, &mut out) } == SUCCESS).then_some((out.used, out.total, None))
    }

    fn u32_of(&self, get: Option<DeviceU32>) -> Option<u32> {
        let get = get?;
        let mut out: c_uint = 0;
        // SAFETY: get takes (device, unsigned int*).
        (unsafe { get(self.device, &mut out) } == SUCCESS).then_some(out)
    }

    fn u32_of_kind(&self, get: Option<DeviceEnumU32>, kind: c_int) -> Option<u32> {
        let get = get?;
        let mut out: c_uint = 0;
        // SAFETY: get takes (device, enum, unsigned int*).
        (unsafe { get(self.device, kind, &mut out) } == SUCCESS).then_some(out)
    }

    fn fan_rpm(&self) -> Option<u32> {
        let get = self.library.get_fan_speed_rpm?;
        let mut out = FanSpeedInfo { version: FAN_SPEED_INFO_V1, fan: 0, speed: 0 };
        // SAFETY: out is a valid nvmlFanSpeedInfo_v1_t with its version set.
        (unsafe { get(self.device, &mut out) } == SUCCESS).then_some(out.speed)
    }

    fn power_constraints_mw(&self) -> Option<(u32, u32)> {
        let get = self.library.get_power_constraints?;
        let (mut min, mut max): (c_uint, c_uint) = (0, 0);
        // SAFETY: get takes (device, unsigned int*, unsigned int*).
        (unsafe { get(self.device, &mut min, &mut max) } == SUCCESS).then_some((min, max))
    }

    /// One running-process list, grown once if NVML asks for more.
    fn process_list(&self, get: Option<ProcessList>) -> Vec<ProcessInfo> {
        let Some(get) = get else { return Vec::new() };
        let mut slots = PROCESS_SLOTS;
        for _ in 0..2 {
            let mut buffer = vec![ProcessInfo::default(); slots];
            let mut count = slots as c_uint;
            // SAFETY: count says how many entries buffer holds.
            let status = unsafe { get(self.device, &mut count, buffer.as_mut_ptr()) };
            match status {
                SUCCESS => {
                    buffer.truncate((count as usize).min(slots));
                    return buffer;
                }
                ERROR_INSUFFICIENT_SIZE => slots = count as usize + 8,
                _ => return Vec::new(),
            }
        }
        Vec::new()
    }

    /// Per-process samples newer than `since_us`. NOT_FOUND (no
    /// process did work since then) and every other failure read as
    /// none.
    fn process_samples(&self, since_us: u64) -> Vec<ProcessUtilizationSample> {
        let Some(get) = self.library.get_process_utilization else { return Vec::new() };
        let mut slots = PROCESS_SLOTS;
        for _ in 0..2 {
            let mut buffer = vec![ProcessUtilizationSample::default(); slots];
            let mut count = slots as c_uint;
            // SAFETY: count says how many entries buffer holds.
            let status = unsafe { get(self.device, buffer.as_mut_ptr(), &mut count, since_us) };
            match status {
                SUCCESS => {
                    buffer.truncate((count as usize).min(slots));
                    return buffer;
                }
                ERROR_INSUFFICIENT_SIZE => slots = count as usize + 8,
                _ => return Vec::new(),
            }
        }
        Vec::new()
    }

    /// CUDA compute capability (8, 6) → "sm_86".
    fn compute_capability(&self) -> Option<String> {
        let get = self.library.get_compute_capability?;
        let (mut major, mut minor): (c_int, c_int) = (0, 0);
        // SAFETY: get takes (device, int*, int*).
        (unsafe { get(self.device, &mut major, &mut minor) } == SUCCESS && major > 0).then(|| format!("sm_{major}{minor}"))
    }

    fn pci(&self) -> Option<PciInfo> {
        let get = self.library.get_pci_info?;
        let mut out = PciInfo::default();
        // SAFETY: out is a valid nvmlPciInfo_t.
        (unsafe { get(self.device, &mut out) } == SUCCESS).then_some(out)
    }

    fn driver_version(&self) -> Option<String> {
        let get = self.library.get_driver_version?;
        let mut buffer = [0 as c_char; DRIVER_VERSION_BUFFER];
        // SAFETY: the buffer is DRIVER_VERSION_BUFFER bytes.
        let status = unsafe { get(buffer.as_mut_ptr(), DRIVER_VERSION_BUFFER as c_uint) };
        (status == SUCCESS).then(|| c_string(&buffer)).filter(|v| !v.is_empty())
    }

    /// The CUDA version the driver supports, as NVML encodes it
    /// (1000 × major + 10 × minor).
    fn cuda_version(&self) -> Option<i32> {
        let get = self.library.get_cuda_version?;
        let mut out: c_int = 0;
        // SAFETY: get takes an int*.
        (unsafe { get(&mut out) } == SUCCESS && out > 0).then_some(out)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session initialized NVML once; this balances it.
        unsafe {
            (self.library.shutdown)();
        }
    }
}

fn c_string(buffer: &[c_char]) -> String {
    let bytes: Vec<u8> = buffer.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

/// Bytes NVML reported for a process, or None for
/// NVML_VALUE_NOT_AVAILABLE (another user's process, MIG, WDDM).
fn process_memory(value: u64) -> Option<u64> {
    (value != VALUE_NOT_AVAILABLE).then_some(value)
}

/// Merge the compute and graphics lists and the busy samples into one
/// row per pid. A process in both lists is one process holding one
/// allocation, so its memory is the larger figure, never the sum.
/// Busy is the newest sample per pid; `with_busy` is false until NVML
/// has handed over a first sample, because that read spans the
/// driver's whole buffer rather than a window of ours.
fn merge_processes(
    lists: &[&[ProcessInfo]],
    samples: &[ProcessUtilizationSample],
    with_busy: bool,
) -> Vec<GpuProcessUsage> {
    let mut by_pid: BTreeMap<i32, GpuProcessUsage> = BTreeMap::new();
    for info in lists.iter().flat_map(|list| list.iter()) {
        let entry = by_pid.entry(info.pid as i32).or_insert_with(|| GpuProcessUsage {
            pid: info.pid as i32,
            ..Default::default()
        });
        if let Some(bytes) = process_memory(info.used_gpu_memory) {
            entry.vram_bytes = entry.vram_bytes.max(bytes);
        }
    }
    if with_busy {
        let mut newest: BTreeMap<i32, (u64, u32)> = BTreeMap::new();
        for sample in samples {
            let slot = newest.entry(sample.pid as i32).or_insert((sample.time_stamp, sample.sm_util));
            if sample.time_stamp >= slot.0 {
                *slot = (sample.time_stamp, sample.sm_util);
            }
        }
        for (pid, (_, sm_util)) in newest {
            let entry = by_pid.entry(pid).or_insert_with(|| GpuProcessUsage { pid, ..Default::default() });
            entry.busy_percent = sm_util.min(100) as f32;
        }
    }
    let mut usages: Vec<GpuProcessUsage> =
        by_pid.into_values().filter(|usage| usage.busy_percent > 0.05 || usage.vram_bytes > 0).collect();
    usages.sort_by(|a, b| {
        (b.busy_percent, b.vram_bytes)
            .partial_cmp(&(a.busy_percent, a.vram_bytes))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    usages
}

/// Collects between two tries at opening NVML while it isn't there
/// (~30 s at the 2 s default).
const REOPEN_EVERY: u32 = 15;

pub struct NvidiaCollector {
    /// Opened on the first collect, never at construction: NVML maps
    /// ~27 MB (measured: a probe peaks at 43 MB with it, 16 MB
    /// without), and a sampler that never wants the nvidia section
    /// shouldn't carry it.
    session: Option<Session>,
    /// Collects left before the next open attempt; 0 = try now. A
    /// recorder that starts before the driver has brought a
    /// compute-only card up (Ben's 3070 initializes after the display
    /// boot) finds it on a later try instead of never.
    reopen_in: u32,
    device_name: String,
    /// NVML's timestamp (µs) of the newest per-process sample read so
    /// far; None before the first collect.
    last_sample_us: Option<u64>,
}

impl Default for NvidiaCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl NvidiaCollector {
    pub fn new() -> Self {
        NvidiaCollector { session: None, reopen_in: 0, device_name: "NVIDIA GPU".to_string(), last_sample_us: None }
    }

    /// Open NVML when there's no session and a try is due. A failed
    /// try where there is no NVIDIA library at all is one failed
    /// dlopen.
    fn ensure_open(&mut self) {
        if self.session.is_some() {
            return;
        }
        if self.reopen_in > 0 {
            self.reopen_in -= 1;
            return;
        }
        self.session = Session::open();
        match &self.session {
            Some(session) => {
                if let Some(name) = session.name() {
                    self.device_name = name;
                }
                self.last_sample_us = None;
            }
            None => self.reopen_in = REOPEN_EVERY - 1,
        }
    }

    pub fn collect(&mut self) -> GpuSnapshot {
        self.ensure_open();
        let Some(session) = &self.session else {
            return GpuSnapshot::default(); // available: false
        };
        let mut snapshot = GpuSnapshot { device_name: self.device_name.clone(), ..Default::default() };

        let utilization = session.utilization();
        let memory = session.memory();
        // A card that answers neither has gone away (driver unloaded,
        // fallen off the bus): say so rather than show zeros.
        snapshot.available = utilization.is_some() || memory.is_some();
        if !snapshot.available {
            return snapshot;
        }
        if let Some(utilization) = utilization {
            snapshot.busy_percent = utilization.gpu.min(100) as f32;
            snapshot.memory_busy_percent = Some(utilization.memory.min(100) as f32);
        }
        if let Some((used, total, reserved)) = memory {
            snapshot.vram_used_bytes = used;
            snapshot.vram_total_bytes = total;
            snapshot.vram_reserved_bytes = reserved;
        }
        snapshot.temperature_core_celsius =
            session.u32_of_kind(session.library.get_temperature, TEMPERATURE_GPU).map(|c| c as f32);
        snapshot.power_draw_watts = session.u32_of(session.library.get_power_usage).map(|mw| mw as f32 / 1000.0);
        snapshot.power_cap_watts = session.u32_of(session.library.get_power_limit).map(|mw| mw as f32 / 1000.0);
        snapshot.core_clock_mhz = session.u32_of_kind(session.library.get_clock, CLOCK_GRAPHICS).map(f64::from);
        snapshot.memory_clock_mhz = session.u32_of_kind(session.library.get_clock, CLOCK_MEM).map(f64::from);
        if snapshot.core_clock_mhz.is_some() {
            snapshot.clock_source = Some("instant read".to_string());
        }
        snapshot.fan_percent = session.u32_of(session.library.get_fan_speed).map(|p| p as f32);
        snapshot.fan_rpm = session.fan_rpm();

        let compute = session.process_list(session.library.get_compute_processes);
        let graphics = session.process_list(session.library.get_graphics_processes);
        let samples = session.process_samples(self.last_sample_us.unwrap_or(0));
        snapshot.processes = merge_processes(&[&compute, &graphics], &samples, self.last_sample_us.is_some());
        // The window opens at the newest sample NVML has handed over,
        // in NVML's own clock (never ours: the header says only "CPU
        // timestamp"). Until a first sample exists every read is the
        // driver's whole buffer, so busy waits for the second one.
        if let Some(newest) = samples.iter().map(|s| s.time_stamp).max() {
            self.last_sample_us = Some(self.last_sample_us.unwrap_or(0).max(newest));
        }
        snapshot
    }
}

/// What the spec sheet needs to know about an NVIDIA card that the
/// PCI tree can't say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NvidiaSpec {
    /// sysfs form, "0000:03:00.0".
    pub pci_address: String,
    pub name: String,
    pub vram_bytes: Option<u64>,
    /// "Ampere".
    pub family: Option<String>,
    /// The CUDA compute capability, "sm_86" (the ISA name, as AMD's
    /// "gfx1201" is).
    pub architecture: Option<String>,
    pub cuda_cores: Option<u32>,
    /// The PCIe generation the card can train to in this slot ("max
    /// possible with this device and system", NVML's words). An idle
    /// NVIDIA card drops its link to Gen 1, so sysfs's current speed
    /// is not the slot's.
    pub link_max_generation: Option<u32>,
    pub power_cap_watts: Option<f32>,
    pub power_cap_max_watts: Option<f32>,
    /// "CUDA 13.4 · driver 615.71.09".
    pub compute_stack: Option<String>,
}

/// The first NVIDIA card's spec details, from a short NVML session.
pub fn spec() -> Option<NvidiaSpec> {
    let session = Session::open()?;
    let pci = session.pci()?;
    let mut spec = NvidiaSpec {
        pci_address: sysfs_pci_address(&c_string(&pci.bus_id))?,
        name: session.name().unwrap_or_else(|| "NVIDIA GPU".to_string()),
        ..Default::default()
    };
    spec.vram_bytes = session.memory().map(|(_, total, _)| total).filter(|&b| b > 0);
    spec.family = session.u32_of(session.library.get_architecture).and_then(architecture_name).map(String::from);
    spec.architecture = session.compute_capability();
    spec.cuda_cores = session.u32_of(session.library.get_num_cores).filter(|&n| n > 0);
    spec.link_max_generation = session.u32_of(session.library.get_max_link_generation).filter(|&g| g > 0);
    spec.power_cap_watts = session.u32_of(session.library.get_power_limit).map(|mw| mw as f32 / 1000.0);
    spec.power_cap_max_watts = session.power_constraints_mw().map(|(_, max)| max as f32 / 1000.0).filter(|&w| w > 0.0);
    spec.compute_stack = compute_stack(session.cuda_version(), session.driver_version());
    Some(spec)
}

/// NVML's bus id ("00000000:03:00.0", an 8-digit domain) in the
/// sysfs form ("0000:03:00.0").
pub fn sysfs_pci_address(bus_id: &str) -> Option<String> {
    let (domain, rest) = bus_id.split_once(':')?;
    let domain = u32::from_str_radix(domain, 16).ok()?;
    Some(format!("{domain:04x}:{}", rest.to_ascii_lowercase()))
}

/// `NVML_DEVICE_ARCH_*`.
pub fn architecture_name(arch: u32) -> Option<&'static str> {
    Some(match arch {
        2 => "Kepler",
        3 => "Maxwell",
        4 => "Pascal",
        5 => "Volta",
        6 => "Turing",
        7 => "Ampere",
        8 => "Ada Lovelace",
        9 => "Hopper",
        10 => "Blackwell",
        13 => "Rubin",
        _ => return None,
    })
}

/// "CUDA 13.4 · driver 615.71.09" from NVML's encoded CUDA version.
pub fn compute_stack(cuda: Option<i32>, driver: Option<String>) -> Option<String> {
    let cuda = cuda.map(|v| format!("CUDA {}.{}", v / 1000, (v % 1000) / 10));
    match (cuda, driver) {
        (Some(cuda), Some(driver)) => Some(format!("{cuda} · driver {driver}")),
        (Some(cuda), None) => Some(cuda),
        (None, Some(driver)) => Some(format!("driver {driver}")),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_nvml_h() {
        // nvml.h (CUDA 13.4.3) on x86_64: unsigned int 4, unsigned
        // long long 8 aligned to 8. A wrong size here is a buffer the
        // driver writes past.
        assert_eq!(size_of::<Utilization>(), 8);
        assert_eq!(size_of::<MemoryV1>(), 24);
        assert_eq!(size_of::<MemoryV2>(), 40, "u32 version, pad, four u64");
        assert_eq!(size_of::<ProcessInfo>(), 24, "pid, pad, u64, two u32");
        assert_eq!(size_of::<ProcessUtilizationSample>(), 32, "pid, pad, u64 ts, four u32");
        assert_eq!(size_of::<FanSpeedInfo>(), 12);
        assert_eq!(size_of::<PciInfo>(), 68, "16 + 5 × u32 + 32");
        assert_eq!(std::mem::offset_of!(MemoryV2, used), 32);
        assert_eq!(std::mem::offset_of!(ProcessUtilizationSample, sm_util), 16);
        assert_eq!(std::mem::offset_of!(PciInfo, bus_id), 36);
    }

    #[test]
    fn struct_versions_follow_nvml_struct_version() {
        assert_eq!(MEMORY_V2, 0x0200_0028, "sizeof 40 | 2 << 24");
        assert_eq!(FAN_SPEED_INFO_V1, 0x0100_000c, "sizeof 12 | 1 << 24");
    }

    fn info(pid: u32, memory: u64) -> ProcessInfo {
        ProcessInfo { pid, used_gpu_memory: memory, ..Default::default() }
    }

    fn sample(pid: u32, time_stamp: u64, sm_util: u32) -> ProcessUtilizationSample {
        ProcessUtilizationSample { pid, time_stamp, sm_util, ..Default::default() }
    }

    #[test]
    fn a_process_in_both_lists_counts_its_memory_once() {
        let compute = [info(10, 776 << 20)];
        let graphics = [info(10, 776 << 20), info(11, 12 << 20)];
        let rows = merge_processes(&[&compute, &graphics], &[], true);
        assert_eq!(rows.len(), 2);
        let row = rows.iter().find(|r| r.pid == 10).unwrap();
        assert_eq!(row.vram_bytes, 776 << 20, "max of the two lists, not their sum");
    }

    #[test]
    fn unavailable_memory_is_not_read_as_a_huge_number() {
        // Another user's process: NVML says NOT_AVAILABLE (u64::MAX).
        let rows = merge_processes(&[&[info(7, VALUE_NOT_AVAILABLE), info(8, 1 << 30)]], &[sample(7, 5, 40)], true);
        let other = rows.iter().find(|r| r.pid == 7).expect("still listed: it is busy");
        assert_eq!(other.vram_bytes, 0);
        assert_eq!(other.busy_percent, 40.0);
    }

    #[test]
    fn busy_is_the_newest_sample_per_process() {
        let rows = merge_processes(&[&[info(5, 1 << 20)]], &[sample(5, 100, 90), sample(5, 300, 20), sample(5, 200, 70)], true);
        assert_eq!(rows[0].busy_percent, 20.0, "timestamp 300 is the newest");
    }

    #[test]
    fn the_first_collect_reports_no_busy_but_keeps_memory() {
        // lastSeen 0 returns the driver's whole buffer: not a window.
        let rows = merge_processes(&[&[info(5, 1 << 20)]], &[sample(5, 100, 90)], false);
        assert_eq!(rows[0].busy_percent, 0.0);
        assert_eq!(rows[0].vram_bytes, 1 << 20);
    }

    #[test]
    fn idle_processes_with_no_memory_are_dropped_and_busiest_lead() {
        let rows = merge_processes(&[&[info(1, 0), info(2, 1 << 20), info(3, 2 << 20)]], &[sample(2, 9, 50)], true);
        let pids: Vec<i32> = rows.iter().map(|r| r.pid).collect();
        assert_eq!(pids, [2, 3], "pid 1 holds nothing and did nothing");
    }

    #[test]
    fn nvml_bus_ids_become_sysfs_addresses() {
        assert_eq!(sysfs_pci_address("00000000:03:00.0").as_deref(), Some("0000:03:00.0"));
        assert_eq!(sysfs_pci_address("00000001:0A:00.1").as_deref(), Some("0001:0a:00.1"));
        assert_eq!(sysfs_pci_address("garbage"), None);
    }

    #[test]
    fn cuda_versions_decode_like_nvidia_smi() {
        assert_eq!(compute_stack(Some(13040), Some("615.71.09".into())).as_deref(), Some("CUDA 13.4 · driver 615.71.09"));
        assert_eq!(compute_stack(Some(12020), None).as_deref(), Some("CUDA 12.2"));
        assert_eq!(compute_stack(None, None), None);
        assert_eq!(architecture_name(7), Some("Ampere"));
        assert_eq!(architecture_name(99), None);
    }

    #[test]
    fn names_stop_at_the_nul() {
        let mut buffer = [0 as c_char; 16];
        for (slot, byte) in buffer.iter_mut().zip(b"RTX 3070\0junk") {
            *slot = *byte as c_char;
        }
        assert_eq!(c_string(&buffer), "RTX 3070");
    }
}
