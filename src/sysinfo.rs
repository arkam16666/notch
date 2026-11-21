//! CPU load, memory used and CPU temperature for the expanded strip.
//!
//! - CPU / RAM: Mach host statistics (public, no process spawning).
//! - Temperature: IOHID temperature sensors (private but long-standing;
//!   what menu-bar monitors use on Apple Silicon). Die sensors are named
//!   "PMU tdie…"; we report the hottest one.

use std::ffi::{c_char, c_void, CString};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// 0..1
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    /// °C
    pub temp: Option<f64>,
}

// ---------------------------------------------------------------- Mach / sysctl

#[repr(C)]
#[derive(Default)]
struct VmStats64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
}

unsafe extern "C" {
    fn mach_host_self() -> u32;
    fn host_statistics(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    fn host_statistics64(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    fn host_page_size(host: u32, size: *mut usize) -> i32;
    fn sysctlbyname(name: *const c_char, old: *mut c_void, oldlen: *mut usize, new: *const c_void, newlen: usize) -> i32;
}

const HOST_CPU_LOAD_INFO: i32 = 3;
const HOST_VM_INFO64: i32 = 4;

fn host() -> u32 {
    static H: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *H.get_or_init(|| unsafe { mach_host_self() })
}

/// Whole-machine CPU busy fraction since the previous call.
fn cpu_usage() -> f64 {
    static PREV: Mutex<Option<[u32; 4]>> = Mutex::new(None);
    let mut ticks = [0u32; 4]; // user, system, idle, nice
    let mut count = 4u32;
    if unsafe { host_statistics(host(), HOST_CPU_LOAD_INFO, ticks.as_mut_ptr().cast(), &mut count) } != 0 {
        return 0.0;
    }
    let mut prev = PREV.lock().unwrap();
    let usage = match *prev {
        Some(p) => {
            let d: Vec<f64> = (0..4).map(|i| ticks[i].wrapping_sub(p[i]) as f64).collect();
            let total = d.iter().sum::<f64>();
            if total > 0.0 { (d[0] + d[1] + d[3]) / total } else { 0.0 }
        }
        None => 0.0,
    };
    *prev = Some(ticks);
    usage
}

/// (used, total) bytes — "Memory Used" like Activity Monitor
/// (app memory + wired + compressed).
fn memory() -> (u64, u64) {
    let mut total = 0u64;
    let mut len = 8usize;
    let name = CString::new("hw.memsize").unwrap();
    unsafe { sysctlbyname(name.as_ptr(), (&mut total as *mut u64).cast(), &mut len, std::ptr::null(), 0) };
    let mut vm = VmStats64::default();
    let mut count = (std::mem::size_of::<VmStats64>() / 4) as u32;
    let mut page = 0usize;
    unsafe {
        host_page_size(host(), &mut page);
        if host_statistics64(host(), HOST_VM_INFO64, (&mut vm as *mut VmStats64).cast(), &mut count) != 0 {
            return (0, total);
        }
    }
    let app = vm.internal_page_count.saturating_sub(vm.purgeable_count) as u64;
    let used = (app + vm.wire_count as u64 + vm.compressor_page_count as u64) * page as u64;
    (used.min(total), total)
}

// ---------------------------------------------------------------- temperature

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventSystemClientCreate(alloc: *const c_void) -> *mut c_void;
    fn IOHIDEventSystemClientSetMatching(client: *mut c_void, matching: *const c_void) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: *mut c_void) -> *const c_void;
    fn IOHIDServiceClientCopyProperty(service: *const c_void, key: *const c_void) -> *const c_void;
    fn IOHIDServiceClientCopyEvent(service: *const c_void, kind: i64, options: i32, ts: i64) -> *const c_void;
    fn IOHIDEventGetFloatValue(event: *const c_void, field: i32) -> f64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

const EVENT_TEMPERATURE: i64 = 15;

/// Hottest CPU die sensor, °C.
fn cpu_temp() -> Option<f64> {
    struct Client(*mut c_void);
    unsafe impl Send for Client {}
    static CLIENT: Mutex<Option<Client>> = Mutex::new(None);
    let mut guard = CLIENT.lock().unwrap();
    let client = match &*guard {
        Some(c) => c.0,
        None => unsafe {
            let c = IOHIDEventSystemClientCreate(std::ptr::null());
            if c.is_null() {
                return None;
            }
            // Apple vendor page 0xff00, usage 5 = temperature sensors
            let keys = [&*NSString::from_str("PrimaryUsagePage"), &*NSString::from_str("PrimaryUsage")];
            let page = NSNumber::new_i32(0xff00);
            let usage = NSNumber::new_i32(5);
            let vals: [&AnyObject; 2] = [&page, &usage];
            let m = NSDictionary::from_slices(&keys, &vals);
            IOHIDEventSystemClientSetMatching(c, Retained::as_ptr(&m).cast());
            *guard = Some(Client(c));
            c
        },
    };
    unsafe {
        let services = IOHIDEventSystemClientCopyServices(client);
        if services.is_null() {
            return None;
        }
        let arr: &NSArray<AnyObject> = &*(services as *const NSArray<AnyObject>);
        let product = NSString::from_str("Product");
        let mut die: Vec<f64> = Vec::new();
        let mut cores: Vec<f64> = Vec::new();
        for svc in arr.iter() {
            let svc_ptr = Retained::as_ptr(&svc) as *const c_void;
            let name_ref = IOHIDServiceClientCopyProperty(svc_ptr, Retained::as_ptr(&product).cast());
            if name_ref.is_null() {
                continue;
            }
            let name = (*(name_ref as *const NSString)).to_string();
            CFRelease(name_ref);
            let ev = IOHIDServiceClientCopyEvent(svc_ptr, EVENT_TEMPERATURE, 0, 0);
            if ev.is_null() {
                continue;
            }
            let t = IOHIDEventGetFloatValue(ev, (EVENT_TEMPERATURE << 16) as i32);
            CFRelease(ev);
            if !(1.0..150.0).contains(&t) {
                continue;
            }
            if name.contains("tdie") {
                die.push(t);
            } else if name.contains("ACC") || name.contains("CPU") || name.contains("SOC") {
                cores.push(t);
            }
        }
        CFRelease(services);
        let pick = if die.is_empty() { cores } else { die };
        pick.into_iter().reduce(f64::max)
    }
}

pub fn current() -> Stats {
    let (mem_used, mem_total) = memory();
    Stats { cpu: cpu_usage(), mem_used, mem_total, temp: cpu_temp() }
}

/// Background loop: sample every 2 s (cheap: no subprocesses).
pub fn run(tx: Sender<Stats>) {
    cpu_usage(); // prime the delta
    loop {
        std::thread::sleep(Duration::from_secs(2));
        if tx.send(current()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sample() {
        super::cpu_usage();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let s = super::current();
        println!(
            "cpu {:.0}%  mem {:.1}/{:.1} GB  temp {:?}",
            s.cpu * 100.0,
            s.mem_used as f64 / 1e9,
            s.mem_total as f64 / 1e9,
            s.temp
        );
        assert!(s.mem_total > 0 && s.mem_used > 0);
    }
}
