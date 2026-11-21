//! Ask Core Audio which apps are currently sending audio to the output.
//! Public API (macOS 14+), no permissions needed. Used to know whether a
//! YouTube tab is actually playing when we can't run JavaScript in it.

use std::ffi::c_void;

use objc2_core_foundation::CFString;

type AudioObjectID = u32;
type OSStatus = i32;

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}

const SYSTEM_OBJECT: AudioObjectID = 1;
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const PROCESS_OBJECT_LIST: u32 = fourcc(b"prs#");
const PROCESS_BUNDLE_ID: u32 = fourcc(b"pbid");
const PROCESS_IS_RUNNING_OUTPUT: u32 = fourcc(b"piro");

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        obj: AudioObjectID,
        addr: *const PropertyAddress,
        qual_size: u32,
        qual: *const c_void,
        out_size: *mut u32,
    ) -> OSStatus;
    fn AudioObjectGetPropertyData(
        obj: AudioObjectID,
        addr: *const PropertyAddress,
        qual_size: u32,
        qual: *const c_void,
        io_size: *mut u32,
        out: *mut c_void,
    ) -> OSStatus;
}

fn addr(selector: u32) -> PropertyAddress {
    PropertyAddress { selector, scope: SCOPE_GLOBAL, element: 0 }
}

fn process_objects() -> Vec<AudioObjectID> {
    let a = addr(PROCESS_OBJECT_LIST);
    let mut size = 0u32;
    unsafe {
        if AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &a, 0, std::ptr::null(), &mut size) != 0 {
            return vec![];
        }
        let mut ids = vec![0 as AudioObjectID; size as usize / 4];
        if AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &a,
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr().cast(),
        ) != 0
        {
            return vec![];
        }
        ids.truncate(size as usize / 4);
        ids
    }
}

fn is_running_output(obj: AudioObjectID) -> bool {
    let a = addr(PROCESS_IS_RUNNING_OUTPUT);
    let mut v = 0u32;
    let mut size = 4u32;
    unsafe {
        AudioObjectGetPropertyData(obj, &a, 0, std::ptr::null(), &mut size, (&mut v as *mut u32).cast()) == 0
            && v != 0
    }
}

fn bundle_id(obj: AudioObjectID) -> Option<String> {
    let a = addr(PROCESS_BUNDLE_ID);
    let mut s: *const CFString = std::ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    unsafe {
        if AudioObjectGetPropertyData(obj, &a, 0, std::ptr::null(), &mut size, (&mut s as *mut *const CFString).cast()) != 0
            || s.is_null()
        {
            return None;
        }
        // The getter follows the Create rule: we own the returned string.
        let owned = objc2_core_foundation::CFRetained::from_raw(std::ptr::NonNull::new_unchecked(s as *mut CFString));
        Some(owned.to_string())
    }
}

const DEFAULT_OUTPUT_DEVICE: u32 = fourcc(b"dOut");
const SCOPE_OUTPUT: u32 = fourcc(b"outp");
const VIRTUAL_MAIN_VOLUME: u32 = fourcc(b"vmvc");
const DEVICE_MUTE: u32 = fourcc(b"mute");

fn get_u32(obj: AudioObjectID, a: &PropertyAddress) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    (unsafe { AudioObjectGetPropertyData(obj, a, 0, std::ptr::null(), &mut size, (&mut v as *mut u32).cast()) } == 0)
        .then_some(v)
}

/// Output volume (0..1) and mute of the default output device. Cheap enough
/// to poll many times a second (used for the volume HUD).
pub fn output_volume() -> Option<(f32, bool)> {
    let dev = get_u32(SYSTEM_OBJECT, &addr(DEFAULT_OUTPUT_DEVICE))?;
    let a = PropertyAddress { selector: VIRTUAL_MAIN_VOLUME, scope: SCOPE_OUTPUT, element: 0 };
    let mut vol = 0f32;
    let mut size = 4u32;
    if unsafe { AudioObjectGetPropertyData(dev, &a, 0, std::ptr::null(), &mut size, (&mut vol as *mut f32).cast()) } != 0 {
        return None;
    }
    let muted = get_u32(dev, &PropertyAddress { selector: DEVICE_MUTE, scope: SCOPE_OUTPUT, element: 0 }).unwrap_or(0) != 0;
    Some((vol, muted))
}

/// Is any process whose bundle id starts with `prefix` playing audio right now?
/// (Chrome plays audio from a helper, e.g. `com.google.Chrome.helper`.)
pub fn is_playing(prefix: &str) -> bool {
    process_objects()
        .into_iter()
        .any(|o| is_running_output(o) && bundle_id(o).is_some_and(|b| b.starts_with(prefix)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn volume() {
        let v = super::output_volume();
        println!("volume = {v:?}");
        assert!(v.is_some());
    }

    #[test]
    fn lists_processes() {
        let ids: Vec<_> = super::process_objects().into_iter().filter_map(super::bundle_id).collect();
        println!("{ids:?}");
        assert!(!ids.is_empty());
    }
}
