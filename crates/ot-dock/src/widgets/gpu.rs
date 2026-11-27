//! GPU usage and temperature for the System meters widget (`SystemSensors`), sampled on a worker thread every
//! 2 s while the widget shows a GPU meter. Usage is the busiest engine (WDDM node) of any adapter from
//! `D3DKMTQueryStatistics`, as Task Manager reports it; temperature comes from `D3DKMTQueryAdapterInfo`
//! (`KMTQAITYPE_ADAPTERPERFDATA`), else NVIDIA's NVML from System32. Offsets follow OpenTaskbar's marshalling.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdapterInfo {
    h_adapter: u32,
    luid_low: u32,
    luid_high: i32,
    num_sources: u32,
    present_move_regions_preferred: i32,
}

#[repr(C)]
struct EnumAdapters2 {
    num_adapters: u32,
    adapters: *mut AdapterInfo,
}

#[repr(C)]
struct CloseAdapter {
    h_adapter: u32,
}

#[repr(C)]
struct QueryAdapterInfo {
    h_adapter: u32,
    kind: u32,
    data: *mut core::ffi::c_void,
    size: u32,
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn D3DKMTEnumAdapters2(args: *mut EnumAdapters2) -> i32;
    fn D3DKMTCloseAdapter(args: *const CloseAdapter) -> i32;
    fn D3DKMTQueryStatistics(args: *mut u8) -> i32;
    fn D3DKMTQueryAdapterInfo(args: *mut QueryAdapterInfo) -> i32;
}

/// `D3DKMT_QUERYSTATISTICS` as OpenTaskbar sizes it.
const STATS_SIZE: usize = 1048;
const STATS_ADAPTER: u32 = 0;
const STATS_NODE: u32 = 5;
/// `KMTQAITYPE_ADAPTERPERFDATA`.
const PERFDATA: u32 = 62;
/// `MaxPlausibleCelsius`.
const MAX_CELSIUS: f64 = 150.0;

fn adapters() -> Vec<AdapterInfo> {
    // SAFETY: the count call takes a null array; the second call fills `list` of that many entries.
    unsafe {
        let mut args = EnumAdapters2 {
            num_adapters: 0,
            adapters: std::ptr::null_mut(),
        };
        if D3DKMTEnumAdapters2(&mut args) != 0 || args.num_adapters == 0 {
            return Vec::new();
        }
        let mut list = vec![AdapterInfo::default(); args.num_adapters as usize];
        args.adapters = list.as_mut_ptr();
        if D3DKMTEnumAdapters2(&mut args) != 0 {
            return Vec::new();
        }
        list.truncate(args.num_adapters as usize);
        list
    }
}

fn close(adapters: &[AdapterInfo]) {
    for a in adapters {
        // SAFETY: handles returned by D3DKMTEnumAdapters2.
        unsafe {
            D3DKMTCloseAdapter(&CloseAdapter {
                h_adapter: a.h_adapter,
            })
        };
    }
}

fn stats_buffer(kind: u32, adapter: &AdapterInfo) -> Vec<u8> {
    let mut buffer = vec![0u8; STATS_SIZE];
    buffer[0..4].copy_from_slice(&kind.to_le_bytes());
    buffer[4..8].copy_from_slice(&adapter.luid_low.to_le_bytes());
    buffer[8..12].copy_from_slice(&adapter.luid_high.to_le_bytes());
    buffer
}

/// Engine running times between samples (`D3dKmtGpuSampler`).
struct Usage {
    /// (LUID, node count) per adapter with 1–127 nodes.
    nodes: Vec<((u32, i32), u32)>,
    last: HashMap<((u32, i32), u32), (i64, Instant)>,
}

impl Usage {
    fn new() -> Option<Self> {
        let list = adapters();
        let mut nodes = Vec::new();
        for a in &list {
            let mut buffer = stats_buffer(STATS_ADAPTER, a);
            // SAFETY: a zeroed buffer of the struct's size with the query filled in.
            if unsafe { D3DKMTQueryStatistics(buffer.as_mut_ptr()) } == 0 {
                let count = u32::from_le_bytes([buffer[28], buffer[29], buffer[30], buffer[31]]);
                if count > 0 && count < 128 {
                    nodes.push(((a.luid_low, a.luid_high), count));
                }
            }
        }
        close(&list);
        (!nodes.is_empty()).then(|| Self {
            nodes,
            last: HashMap::new(),
        })
    }

    /// The busiest engine since the last sample, 0..1; `None` on the first sample or when nothing answered.
    fn sample(&mut self) -> Option<f64> {
        let mut busiest: Option<f64> = None;
        for &((low, high), count) in &self.nodes {
            let adapter = AdapterInfo {
                luid_low: low,
                luid_high: high,
                ..Default::default()
            };
            for node in 0..count {
                let mut buffer = stats_buffer(STATS_NODE, &adapter);
                buffer[800..804].copy_from_slice(&node.to_le_bytes());
                // SAFETY: as above.
                if unsafe { D3DKMTQueryStatistics(buffer.as_mut_ptr()) } != 0 {
                    continue;
                }
                let mut running = [0u8; 8];
                running.copy_from_slice(&buffer[24..32]);
                let running = i64::from_le_bytes(running);
                let now = Instant::now();
                let key = ((low, high), node);
                if let Some((previous, at)) = self.last.insert(key, (running, now)) {
                    let elapsed = now.duration_since(at).as_nanos() as f64 / 100.0;
                    let delta = running - previous;
                    if delta >= 0 && elapsed > 0.0 {
                        busiest = Some(busiest.unwrap_or(0.0).max(delta as f64 / elapsed));
                    }
                }
            }
        }
        busiest.map(|v| v.clamp(0.0, 1.0))
    }
}

/// The hottest adapter in °C from WDDM performance data.
fn wddm_temperature() -> Option<f64> {
    let list = adapters();
    let mut hottest: Option<f64> = None;
    for a in &list {
        let mut perf = [0u8; 64];
        let mut args = QueryAdapterInfo {
            h_adapter: a.h_adapter,
            kind: PERFDATA,
            data: perf.as_mut_ptr().cast(),
            size: perf.len() as u32,
        };
        // SAFETY: a 64-byte D3DKMT_ADAPTER_PERFDATA buffer.
        if unsafe { D3DKMTQueryAdapterInfo(&mut args) } == 0 {
            let deci = u32::from_le_bytes([perf[56], perf[57], perf[58], perf[59]]);
            let celsius = f64::from(deci) / 10.0;
            if celsius > 0.0 && celsius < MAX_CELSIUS {
                hottest = Some(hottest.map_or(celsius, |h: f64| h.max(celsius)));
            }
        }
    }
    close(&list);
    hottest
}

/// NVIDIA's management library, loaded from System32 only.
struct Nvml {
    library: windows_sys::Win32::Foundation::HMODULE,
    count: unsafe extern "C" fn(*mut u32) -> i32,
    handle: unsafe extern "C" fn(u32, *mut *mut core::ffi::c_void) -> i32,
    temperature: unsafe extern "C" fn(*mut core::ffi::c_void, i32, *mut u32) -> i32,
    shutdown: unsafe extern "C" fn() -> i32,
}

impl Nvml {
    fn load() -> Option<Self> {
        use windows_sys::Win32::Foundation::FreeLibrary;
        use windows_sys::Win32::System::LibraryLoader::{
            GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
        };
        let name: Vec<u16> = "nvml.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: loading a System32 library and resolving documented exports by name.
        unsafe {
            let library = LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            );
            if library.is_null() {
                return None;
            }
            let find = |name: &[u8]| GetProcAddress(library, name.as_ptr());
            let (Some(init), Some(count), Some(handle), Some(temperature), Some(shutdown)) = (
                find(b"nvmlInit_v2\0"),
                find(b"nvmlDeviceGetCount_v2\0"),
                find(b"nvmlDeviceGetHandleByIndex_v2\0"),
                find(b"nvmlDeviceGetTemperature\0"),
                find(b"nvmlShutdown\0"),
            ) else {
                FreeLibrary(library);
                return None;
            };
            let init: unsafe extern "C" fn() -> i32 = std::mem::transmute(init);
            if init() != 0 {
                FreeLibrary(library);
                return None;
            }
            Some(Self {
                library,
                count: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "C" fn(*mut u32) -> i32,
                >(count),
                handle: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "C" fn(u32, *mut *mut core::ffi::c_void) -> i32,
                >(handle),
                temperature: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "C" fn(*mut core::ffi::c_void, i32, *mut u32) -> i32,
                >(temperature),
                shutdown: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "C" fn() -> i32,
                >(shutdown),
            })
        }
    }

    fn hottest(&self) -> Option<f64> {
        // SAFETY: NVML calls after a successful init.
        unsafe {
            let mut n = 0u32;
            if (self.count)(&mut n) != 0 {
                return None;
            }
            let mut hottest: Option<u32> = None;
            for i in 0..n {
                let mut device = std::ptr::null_mut();
                let mut t = 0u32;
                if (self.handle)(i, &mut device) == 0 && (self.temperature)(device, 0, &mut t) == 0
                {
                    hottest = Some(hottest.unwrap_or(0).max(t));
                }
            }
            hottest.map(f64::from)
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: balances the init; the library was loaded above.
        unsafe {
            (self.shutdown)();
            windows_sys::Win32::Foundation::FreeLibrary(self.library);
        }
    }
}

/// The latest readings and how many samples were taken.
#[derive(Clone, Copy, Default)]
pub struct Readings {
    pub usage: Option<f64>,
    pub celsius: Option<f64>,
    pub samples: u64,
}

struct Shared {
    readings: Mutex<Readings>,
    stop: AtomicBool,
}

/// The sampling thread; dropping it stops the thread.
pub struct Sensors {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Sensors {
    pub fn start(usage: bool, temperature: bool) -> Self {
        let shared = Arc::new(Shared {
            readings: Mutex::new(Readings::default()),
            stop: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("gpu-sensors".into())
            .spawn(move || run(&worker, usage, temperature))
            .ok();
        Self { shared, thread }
    }

    pub fn readings(&self) -> Readings {
        self.shared.readings.lock().map(|r| *r).unwrap_or_default()
    }
}

impl Drop for Sensors {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared, want_usage: bool, want_temperature: bool) {
    let mut usage = if want_usage { Usage::new() } else { None };
    let mut nvml: Option<Nvml> = None;
    let mut wddm_temp = want_temperature && wddm_temperature().is_some();
    if want_temperature && !wddm_temp {
        nvml = Nvml::load();
    }
    // `PrimeSensorBaselineIfNeeded`: a baseline, 350 ms, then the first reading.
    if let Some(u) = usage.as_mut() {
        u.sample();
    }
    std::thread::park_timeout(Duration::from_millis(350));
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        let usage_now = usage.as_mut().and_then(Usage::sample);
        let celsius = if !want_temperature {
            None
        } else if wddm_temp {
            let t = wddm_temperature();
            wddm_temp = t.is_some() || nvml.is_none();
            t
        } else {
            nvml.as_ref().and_then(Nvml::hottest)
        };
        if let Ok(mut r) = shared.readings.lock() {
            r.usage = usage_now;
            r.celsius = celsius;
            r.samples += 1;
        }
        // `SensorSamplePeriodMs`.
        std::thread::park_timeout(Duration::from_millis(1900));
    }
}
