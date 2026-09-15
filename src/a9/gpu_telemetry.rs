//! NVIDIA clocks, temperature and power through NVML, opened at run time.
//! No driver, no NVML, or any failure along the way: empty readings and
//! nothing else -- the hashrate is measured by the miner itself, this only
//! decorates it.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use libloading::{Library, Symbol};

use crate::a9::miner::status::DeviceFigures;

/// What NVML says about one device.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub name: String,
    pub core_mhz: Option<u32>,
    pub mem_mhz: Option<u32>,
    pub temp_c: Option<u32>,
    pub power_w: Option<f64>,
}

type Handle = *mut std::ffi::c_void;
type ClockFn = unsafe extern "C" fn(Handle, u32, *mut u32) -> i32;
const NVML_CLOCK_SM: u32 = 1;
const NVML_CLOCK_MEM: u32 = 2;
const NVML_TEMPERATURE_GPU: u32 = 0;

/// The six entry points used, resolved once. The library handle is kept
/// so the function pointers stay valid.
struct Nvml {
    _lib: Library,
    count: unsafe extern "C" fn(*mut u32) -> i32,
    handle: unsafe extern "C" fn(u32, *mut Handle) -> i32,
    name: unsafe extern "C" fn(Handle, *mut u8, u32) -> i32,
    clock: ClockFn,
    temp: ClockFn,
    power: unsafe extern "C" fn(Handle, *mut u32) -> i32,
}

// SAFETY: NVML is documented thread-safe after nvmlInit, and the struct
// holds only the library and plain function pointers.
unsafe impl Send for Nvml {}
unsafe impl Sync for Nvml {}

#[cfg(windows)]
const CANDIDATES: &[&str] = &["nvml.dll"];
#[cfg(not(windows))]
const CANDIDATES: &[&str] = &["libnvidia-ml.so.1", "libnvidia-ml.so"];

fn load() -> Option<Nvml> {
    // SAFETY: loading a system library and resolving documented C symbols
    // with their documented signatures. A wrong library merely fails here.
    unsafe {
        let lib = CANDIDATES.iter().find_map(|n| Library::new(n).ok())?;
        let init: Symbol<unsafe extern "C" fn() -> i32> = lib.get(b"nvmlInit_v2\0").ok()?;
        if init() != 0 {
            return None;
        }
        let count = *lib
            .get::<unsafe extern "C" fn(*mut u32) -> i32>(b"nvmlDeviceGetCount_v2\0")
            .ok()?;
        let handle = *lib
            .get::<unsafe extern "C" fn(u32, *mut Handle) -> i32>(
                b"nvmlDeviceGetHandleByIndex_v2\0",
            )
            .ok()?;
        let name = *lib
            .get::<unsafe extern "C" fn(Handle, *mut u8, u32) -> i32>(b"nvmlDeviceGetName\0")
            .ok()?;
        let clock = *lib.get::<ClockFn>(b"nvmlDeviceGetClockInfo\0").ok()?;
        let temp = *lib.get::<ClockFn>(b"nvmlDeviceGetTemperature\0").ok()?;
        let power = *lib
            .get::<unsafe extern "C" fn(Handle, *mut u32) -> i32>(b"nvmlDeviceGetPowerUsage\0")
            .ok()?;
        Some(Nvml {
            _lib: lib,
            count,
            handle,
            name,
            clock,
            temp,
            power,
        })
    }
}

static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
static CACHE: Mutex<Option<(Instant, Vec<Reading>)>> = Mutex::new(None);

fn read_all(n: &Nvml) -> Vec<Reading> {
    let mut count = 0u32;
    // SAFETY: calls with out-pointers to live locals and handles NVML
    // itself handed out; every result is checked before use.
    if unsafe { (n.count)(&mut count) } != 0 {
        return Vec::new();
    }
    (0..count)
        .filter_map(|i| unsafe {
            let mut h: Handle = std::ptr::null_mut();
            if (n.handle)(i, &mut h) != 0 {
                return None;
            }
            let mut buf = [0u8; 96];
            let name = if (n.name)(h, buf.as_mut_ptr(), buf.len() as u32) == 0 {
                let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                String::from_utf8_lossy(&buf[..end]).into_owned()
            } else {
                format!("NVIDIA GPU {i}")
            };
            let get = |f: ClockFn, kind: u32| {
                let mut v = 0u32;
                (f(h, kind, &mut v) == 0).then_some(v)
            };
            let mut mw = 0u32;
            let power_w = ((n.power)(h, &mut mw) == 0).then(|| f64::from(mw) / 1000.0);
            Some(Reading {
                name,
                core_mhz: get(n.clock, NVML_CLOCK_SM),
                mem_mhz: get(n.clock, NVML_CLOCK_MEM),
                temp_c: get(n.temp, NVML_TEMPERATURE_GPU),
                power_w,
            })
        })
        .collect()
}

/// Every NVIDIA device the driver knows, refreshed at most once a second
/// -- the stats server asks on every poll.
pub fn readings() -> Vec<Reading> {
    let Some(n) = NVML.get_or_init(load) else {
        return Vec::new();
    };
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, r)) = &*cache {
        if at.elapsed() < Duration::from_secs(1) {
            return r.clone();
        }
    }
    let fresh = read_all(n);
    *cache = Some((Instant::now(), fresh.clone()));
    fresh
}

fn bare(name: &str) -> &str {
    name.strip_prefix("NVIDIA ").unwrap_or(name).trim()
}

/// The k-th device named X takes the k-th reading named X. wgpu's adapter
/// order and NVML's device order both follow the PCI bus in practice, and
/// the name is the only thing the two share; two identical cards are
/// therefore paired by position. Devices with no reading keep their `None`s.
pub fn merge(devices: &mut [DeviceFigures], readings: &[Reading]) {
    let mut used = vec![false; readings.len()];
    for d in devices.iter_mut() {
        let pick = readings
            .iter()
            .enumerate()
            .find(|(j, r)| !used[*j] && bare(&r.name) == bare(&d.name));
        if let Some((j, r)) = pick {
            used[j] = true;
            d.core_mhz = r.core_mhz;
            d.mem_mhz = r.mem_mhz;
            d.temp_c = r.temp_c;
            d.power_w = r.power_w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a9::miner::status::DeviceFigures;

    fn dev(i: u32, name: &str) -> DeviceFigures {
        DeviceFigures {
            index: i,
            name: name.into(),
            hps: 0.0,
            hashes: 0,
            core_mhz: None,
            mem_mhz: None,
            temp_c: None,
            power_w: None,
        }
    }

    fn reading(name: &str, core: u32) -> Reading {
        Reading {
            name: name.into(),
            core_mhz: Some(core),
            mem_mhz: Some(14001),
            temp_c: Some(50),
            power_w: Some(100.0),
        }
    }

    // Two identical cards: the k-th adapter named X takes the k-th NVML
    // device named X. A card of another vendor is left untouched.
    #[test]
    fn readings_pair_with_devices_by_name_in_order() {
        let mut devs = vec![
            dev(0, "NVIDIA GeForce RTX 5090"),
            dev(1, "NVIDIA GeForce RTX 5090"),
            dev(2, "Intel Arc"),
        ];
        let reads = vec![
            reading("NVIDIA GeForce RTX 5090", 2500),
            reading("NVIDIA GeForce RTX 5090", 2400),
        ];
        merge(&mut devs, &reads);
        assert_eq!(devs[0].core_mhz, Some(2500));
        assert_eq!(devs[1].core_mhz, Some(2400));
        assert_eq!(devs[2].core_mhz, None);
        assert_eq!(devs[0].power_w, Some(100.0));
    }

    // wgpu and NVML do not always agree on the "NVIDIA " prefix.
    #[test]
    fn the_vendor_prefix_does_not_get_in_the_way() {
        let mut devs = vec![dev(0, "GeForce RTX 5090")];
        merge(&mut devs, &[reading("NVIDIA GeForce RTX 5090", 2500)]);
        assert_eq!(devs[0].core_mhz, Some(2500));
    }

    /// Needs the NVIDIA driver; skipped with a note otherwise.
    #[test]
    fn the_driver_when_present_reports_a_clock() {
        let r = readings();
        if r.is_empty() {
            eprintln!("NVML not available here; skipped");
            return;
        }
        assert!(r[0].core_mhz.unwrap_or(0) > 0, "{r:?}");
        assert!(!r[0].name.is_empty());
    }
}
