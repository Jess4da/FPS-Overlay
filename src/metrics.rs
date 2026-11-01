//! Native metrics: CPU, RAM, GPU/VRAM via PDH, GPU temp/clock via NVML.
//! Everything is lazy: `new()` only zeroes fields; the first `poll()` picks the
//! DXGI adapter, opens PDH, and (only for NVIDIA) loads nvml.dll.

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{ERROR_SUCCESS, FILETIME, FreeLibrary, HMODULE};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::core::{PCSTR, PCWSTR};

#[derive(Debug, Clone, Copy, Default)]
pub struct Sample {
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
    pub gpu_temp_c: Option<f32>,
    pub gpu_mhz: Option<f32>,
    pub vram_used_gb: Option<f32>,
    pub vram_total_gb: Option<f32>,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
}

pub struct Metrics {
    started: bool,
    prev: Option<(u64, u64, u64)>,
    adapter: Option<Adapter>,
    pdh: Option<PdhState>,
    nvml: Option<Nvml>,
}

struct Adapter {
    /// Lowercased `luid_tag`: matched against PDH instance names.
    tag: String,
    vram_total: u64,
    nvidia: bool,
}

struct PdhState {
    query: PDH_HQUERY,
    engine: PDH_HCOUNTER,
    vram: PDH_HCOUNTER,
    tag: String,
    buf: Vec<u64>, // 8-byte aligned staging buffer for PDH_FMT_COUNTERVALUE_ITEM_W, grow only
}

impl PdhState {
    fn new(tag: &str) -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        let mut engine = PDH_HCOUNTER::default();
        let mut vram = PDH_HCOUNTER::default();
        unsafe {
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != ERROR_SUCCESS.0 {
                return None;
            }
            let ok = PdhAddEnglishCounterW(
                query,
                windows::core::w!("\\GPU Engine(*engtype_3D)\\Utilization Percentage"),
                0,
                &mut engine,
            ) == ERROR_SUCCESS.0
                && PdhAddEnglishCounterW(
                    query,
                    windows::core::w!("\\GPU Adapter Memory(*)\\Dedicated Usage"),
                    0,
                    &mut vram,
                ) == ERROR_SUCCESS.0;
            if !ok {
                PdhCloseQuery(query);
                return None;
            }
            let _ = PdhCollectQueryData(query);
        }
        Some(Self {
            query,
            engine,
            vram,
            tag: tag.to_string(),
            buf: Vec::new(),
        })
    }
}

impl Drop for PdhState {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

type NvmlFn = unsafe extern "system" fn(*mut c_void, i32, *mut u32) -> i32;

struct Nvml {
    lib: HMODULE,
    device: *mut c_void,
    get_temp: NvmlFn,
    get_clock: NvmlFn,
}

impl Nvml {
    fn load() -> Option<Self> {
        unsafe {
            let lib = LoadLibraryW(windows::core::w!("nvml.dll")).ok()?;
            let init: unsafe extern "system" fn() -> i32 =
                sym(lib, windows::core::s!("nvmlInit_v2"))?;
            let get_handle: unsafe extern "system" fn(u32, *mut *mut c_void) -> i32 =
                sym(lib, windows::core::s!("nvmlDeviceGetHandleByIndex_v2"))?;
            let get_temp = sym(lib, windows::core::s!("nvmlDeviceGetTemperature"))?;
            let get_clock = sym(lib, windows::core::s!("nvmlDeviceGetClockInfo"))?;
            let mut device = std::ptr::null_mut();
            if init() != 0 || get_handle(0, &mut device) != 0 {
                let _ = FreeLibrary(lib);
                return None;
            }
            Some(Self {
                lib,
                device,
                get_temp,
                get_clock,
            })
        }
    }

    fn read(&self, f: NvmlFn, mode: i32) -> Option<f32> {
        let mut v = 0u32;
        // SAFETY: `f` was resolved from nvml.dll and `device` came from nvmlDeviceGetHandleByIndex_v2.
        (unsafe { f(self.device, mode, &mut v) } == 0).then_some(v as f32)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.lib);
        }
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started: false,
            prev: None,
            adapter: None,
            pdh: None,
            nvml: None,
        }
    }

    pub fn poll(&mut self) -> Sample {
        if !self.started {
            self.started = true;
            if let Some(a) = pick_adapter() {
                self.pdh = PdhState::new(&a.tag);
                if a.nvidia {
                    self.nvml = Nvml::load();
                }
                self.adapter = Some(a);
            }
        }

        let mut s = Sample::default();

        if let Some(now) = system_times() {
            if let Some(p) = self.prev {
                s.cpu = Some(cpu_percent(
                    now.0.saturating_sub(p.0),
                    now.1.saturating_sub(p.1),
                    now.2.saturating_sub(p.2),
                ));
            }
            self.prev = Some(now);
        }

        if let Some((used, total)) = ram() {
            s.ram_used_gb = used;
            s.ram_total_gb = total;
        }

        if let (Some(pdh), Some(ad)) = (self.pdh.as_mut(), self.adapter.as_ref()) {
            let _ = unsafe { PdhCollectQueryData(pdh.query) };
            let PdhState {
                buf,
                tag,
                engine,
                vram,
                ..
            } = pdh;
            if let Some(v) = sum_instances(buf, *engine, tag) {
                s.gpu = Some((v as f32).clamp(0.0, 100.0));
            }
            if let Some(v) = sum_instances(buf, *vram, tag) {
                s.vram_used_gb = Some(gib(v));
            }
            s.vram_total_gb = Some(gib(ad.vram_total as f64));
        }

        if let Some(n) = &self.nvml {
            s.gpu_temp_c = n.read(n.get_temp, 0); // NVML_TEMPERATURE_GPU
            s.gpu_mhz = n.read(n.get_clock, 0); // NVML_CLOCK_GRAPHICS
        }

        s
    }
}

/// `"luid_0x{high:08X}_0x{low:08X}"` — the LUID spelling PDH instance names contain.
pub fn luid_tag(high: i32, low: u32) -> String {
    format!("luid_0x{high:08X}_0x{low:08X}")
}

/// `(1 - idle / (kernel + user)) * 100`, clamped 0..=100.
pub fn cpu_percent(idle: u64, kernel: u64, user: u64) -> f32 {
    let total = kernel.saturating_add(user);
    if total == 0 {
        return 0.0;
    }
    (((1.0 - idle as f64 / total as f64) * 100.0) as f32).clamp(0.0, 100.0)
}

/// Adapter with the largest dedicated video memory, skipping software adapters.
/// The DXGI factory is dropped on return.
fn pick_adapter() -> Option<Adapter> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().ok()? };
    let mut best = None;
    for i in 0.. {
        let Ok(a) = (unsafe { factory.EnumAdapters1(i) }) else {
            break;
        };
        let Ok(d) = (unsafe { a.GetDesc1() }) else {
            continue;
        };
        if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|b: &windows::Win32::Graphics::Dxgi::DXGI_ADAPTER_DESC1| {
                d.DedicatedVideoMemory > b.DedicatedVideoMemory
            })
        {
            best = Some(d);
        }
    }
    let d = best?;
    Some(Adapter {
        tag: luid_tag(d.AdapterLuid.HighPart, d.AdapterLuid.LowPart).to_ascii_lowercase(),
        vram_total: d.DedicatedVideoMemory as u64,
        nvidia: d.VendorId == 0x10DE,
    })
}

/// Sum of the named counter instances whose instance name contains `tag`
/// (ASCII case-insensitive). `None` when PDH has no matching data.
fn sum_instances(buf: &mut Vec<u64>, counter: PDH_HCOUNTER, tag: &str) -> Option<f64> {
    let mut size = 0u32;
    let mut count = 0u32;
    let mut rc = unsafe {
        PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None)
    };
    if rc == PDH_MORE_DATA {
        buf.resize(size.div_ceil(8) as usize, 0);
        let mut size = (buf.len() * 8) as u32;
        rc = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                Some(buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
            )
        };
    }
    if rc != ERROR_SUCCESS.0 || count == 0 {
        return None;
    }
    // SAFETY: PDH filled `count` items of PDH_FMT_COUNTERVALUE_ITEM_W into our 8-byte-aligned buffer.
    let items = unsafe {
        std::slice::from_raw_parts(
            buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W,
            count as usize,
        )
    };
    let mut total = 0.0;
    let mut found = false;
    for it in items {
        if it.FmtValue.CStatus != ERROR_SUCCESS.0 || !contains_ci(it.szName.0, tag.as_bytes()) {
            continue;
        }
        total += unsafe { it.FmtValue.Anonymous.doubleValue };
        found = true;
    }
    found.then_some(total)
}

/// ASCII case-insensitive `haystack.contains(needle)` for a NUL-terminated wide string.
fn contains_ci(hay: *const u16, needle: &[u8]) -> bool {
    let mut len = 0usize;
    // SAFETY: instance names from PDH are NUL-terminated wide strings.
    unsafe {
        while *hay.add(len) != 0 {
            len += 1;
        }
    }
    needle.len() <= len
        && (0..=len - needle.len()).any(|i| {
            needle.iter().enumerate().all(|(j, &c)| unsafe {
                let w = *hay.add(i + j);
                w < 0x80 && (w as u8).eq_ignore_ascii_case(&c)
            })
        })
}

fn system_times() -> Option<(u64, u64, u64)> {
    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()? };
    Some((ft(idle), ft(kernel), ft(user)))
}

fn ft(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

fn ram() -> Option<(f32, f32)> {
    let mut m = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    unsafe { GlobalMemoryStatusEx(&mut m).ok()? };
    let total = m.ullTotalPhys as f64;
    Some((gib(total - m.ullAvailPhys as f64), gib(total)))
}

fn gib(bytes: f64) -> f32 {
    (bytes / (1024.0 * 1024.0 * 1024.0)) as f32
}

unsafe fn sym<T: Copy>(lib: HMODULE, name: PCSTR) -> Option<T> {
    let p = unsafe { GetProcAddress(lib, name) }?;
    Some(unsafe { std::mem::transmute_copy(&p) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn luid_tag_format() {
        assert_eq!(luid_tag(0, 0x1234), "luid_0x00000000_0x00001234");
    }

    #[test]
    fn contains_ci_works() {
        let name: Vec<u16> = "pid_7_luid_0x000151E4_phys_0_engtype_3D\0"
            .encode_utf16()
            .collect();
        assert!(contains_ci(name.as_ptr(), b"luid_0x000151e4"));
        assert!(!contains_ci(name.as_ptr(), b"luid_0x00018cd5"));
    }

    #[test]
    fn cpu_percent_math() {
        assert!((cpu_percent(25, 80, 20) - 75.0).abs() < 1e-4);
        assert_eq!(cpu_percent(0, 0, 0), 0.0);
    }

    #[test]
    fn new_is_cheap() {
        let m = Metrics::new();
        assert!(m.pdh.is_none(), "new() must not open PDH");
        assert!(m.nvml.is_none(), "new() must not load NVML");
    }

    #[test]
    fn poll_smoke() {
        let mut m = Metrics::new();
        let _ = m.poll();
        std::thread::sleep(Duration::from_millis(1100));
        let t = Instant::now();
        let s = m.poll();
        let us = t.elapsed().as_micros();
        println!("sample: {s:?}");
        println!("poll took {us} us");
        assert!(s.ram_total_gb > 0.0);
        let cpu = s.cpu.expect("cpu after second poll");
        assert!((0.0..=100.0).contains(&cpu), "cpu out of range: {cpu}");
    }
}
