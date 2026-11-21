//! "Charger plugged in" detection via IOKit power-source notifications
//! (event driven — fires the moment the power source changes).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_foundation::NSString;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPSNotificationCreateRunLoopSource(
        callback: extern "C" fn(*mut c_void),
        context: *mut c_void,
    ) -> *mut c_void;
    fn IOPSCopyPowerSourcesInfo() -> *const c_void;
    fn IOPSGetProvidingPowerSourceType(snapshot: *const c_void) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopCommonModes: *const c_void;
}

static CHANGED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_change(_ctx: *mut c_void) {
    CHANGED.store(true, Ordering::SeqCst);
}

/// Start listening (call once, on the main thread).
pub fn watch() {
    unsafe {
        let src = IOPSNotificationCreateRunLoopSource(on_change, std::ptr::null_mut());
        if !src.is_null() {
            CFRunLoopAddSource(CFRunLoopGetMain(), src, kCFRunLoopCommonModes);
        }
    }
}

/// True once after each power-source change.
pub fn take_changed() -> bool {
    CHANGED.swap(false, Ordering::SeqCst)
}

/// Is the Mac running from the power adapter right now?
pub fn on_ac() -> bool {
    unsafe {
        let snap = IOPSCopyPowerSourcesInfo();
        if snap.is_null() {
            return false;
        }
        let ty = IOPSGetProvidingPowerSourceType(snap); // not owned (Get rule)
        let ac = !ty.is_null() && (*(ty as *const NSString)).to_string() == "AC Power";
        CFRelease(snap);
        ac
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_pmset() {
        let out = std::process::Command::new("/usr/bin/pmset").args(["-g", "batt"]).output().unwrap();
        let pm_ac = String::from_utf8_lossy(&out.stdout).contains("'AC Power'");
        assert_eq!(super::on_ac(), pm_ac);
        println!("on_ac = {}", super::on_ac());
    }
}
