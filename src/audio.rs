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

// ---------------------------------------------------------------------------
// Output devices (speaker picker on the music page)

const DEVICES: u32 = fourcc(b"dev#");
const STREAMS: u32 = fourcc(b"stm#");
const NAME: u32 = fourcc(b"lnam");
const TRANSPORT: u32 = fourcc(b"tran");
const HIDDEN: u32 = fourcc(b"hidn");
const DATA_SOURCES: u32 = fourcc(b"ssc#");
const DATA_SOURCE: u32 = fourcc(b"ssrc");
const DATA_SOURCE_NAME: u32 = fourcc(b"lscn");

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectSetPropertyData(
        obj: AudioObjectID,
        addr: *const PropertyAddress,
        qual_size: u32,
        qual: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OSStatus;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    BuiltIn,
    Headphones,
    Bluetooth,
    AirPlay,
    Display,
    Usb,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub device: u32,
    /// AirPlay receivers are "data sources" of the single AirPlay device
    pub source: Option<u32>,
    pub name: String,
    pub kind: Kind,
    pub current: bool,
}

fn get_u32s(obj: AudioObjectID, a: &PropertyAddress) -> Vec<u32> {
    let mut size = 0u32;
    unsafe {
        if AudioObjectGetPropertyDataSize(obj, a, 0, std::ptr::null(), &mut size) != 0 || size == 0 {
            return vec![];
        }
        let mut v = vec![0u32; size as usize / 4];
        if AudioObjectGetPropertyData(obj, a, 0, std::ptr::null(), &mut size, v.as_mut_ptr().cast()) != 0 {
            return vec![];
        }
        v.truncate(size as usize / 4);
        v
    }
}

fn get_string(obj: AudioObjectID, a: &PropertyAddress) -> Option<String> {
    let mut s: *const CFString = std::ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    unsafe {
        if AudioObjectGetPropertyData(obj, a, 0, std::ptr::null(), &mut size, (&mut s as *mut *const CFString).cast()) != 0
            || s.is_null()
        {
            return None;
        }
        let owned = objc2_core_foundation::CFRetained::from_raw(std::ptr::NonNull::new_unchecked(s as *mut CFString));
        Some(owned.to_string())
    }
}

#[repr(C)]
struct ValueTranslation {
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    output_size: u32,
}

fn source_name(dev: AudioObjectID, id: u32) -> Option<String> {
    let a = PropertyAddress { selector: DATA_SOURCE_NAME, scope: SCOPE_OUTPUT, element: 0 };
    let mut out: *const CFString = std::ptr::null();
    let mut t = ValueTranslation {
        input: (&id as *const u32).cast(),
        input_size: 4,
        output: (&mut out as *mut *const CFString).cast(),
        output_size: std::mem::size_of::<*const CFString>() as u32,
    };
    let mut size = std::mem::size_of::<ValueTranslation>() as u32;
    unsafe {
        if AudioObjectGetPropertyData(dev, &a, 0, std::ptr::null(), &mut size, (&mut t as *mut ValueTranslation).cast()) != 0
            || out.is_null()
        {
            return None;
        }
        let owned = objc2_core_foundation::CFRetained::from_raw(std::ptr::NonNull::new_unchecked(out as *mut CFString));
        Some(owned.to_string())
    }
}

/// Every place sound can go right now: built-in speakers, headphones,
/// Bluetooth / USB / display speakers and AirPlay receivers.
pub fn outputs() -> Vec<Output> {
    let current = get_u32(SYSTEM_OBJECT, &addr(DEFAULT_OUTPUT_DEVICE)).unwrap_or(0);
    let mut out = Vec::new();
    for dev in get_u32s(SYSTEM_OBJECT, &addr(DEVICES)) {
        let streams = get_u32s(dev, &PropertyAddress { selector: STREAMS, scope: SCOPE_OUTPUT, element: 0 });
        if streams.is_empty() || get_u32(dev, &addr(HIDDEN)).unwrap_or(0) != 0 {
            continue;
        }
        let transport = get_u32(dev, &addr(TRANSPORT)).unwrap_or(0);
        if transport == fourcc(b"grup") {
            continue; // aggregate devices (incl. our own audio tap)
        }
        let name = get_string(dev, &addr(NAME)).unwrap_or_default();
        let src_addr = PropertyAddress { selector: DATA_SOURCE, scope: SCOPE_OUTPUT, element: 0 };
        let kind = match &transport.to_be_bytes() {
            b"bltn" => {
                // built-in jack reports "headphones" as its data source
                let src = get_u32(dev, &src_addr).unwrap_or(0);
                if src == fourcc(b"hdpn") { Kind::Headphones } else { Kind::BuiltIn }
            }
            b"blue" | b"blea" => Kind::Bluetooth,
            b"airp" => Kind::AirPlay,
            b"hdmi" | b"dprt" | b"thun" => Kind::Display,
            b"usb " => Kind::Usb,
            _ => Kind::Other,
        };
        if kind == Kind::AirPlay {
            // one entry per AirPlay receiver
            let cur_src = get_u32(dev, &src_addr);
            let ids = get_u32s(dev, &PropertyAddress { selector: DATA_SOURCES, scope: SCOPE_OUTPUT, element: 0 });
            for id in ids {
                if let Some(n) = source_name(dev, id) {
                    out.push(Output { device: dev, source: Some(id), name: n, kind, current: dev == current && cur_src == Some(id) });
                }
            }
            continue;
        }
        out.push(Output { device: dev, source: None, name, kind, current: dev == current });
    }
    // current first-ish order: built-in, headphones, others by name
    out.sort_by_key(|o| (o.kind != Kind::BuiltIn && o.kind != Kind::Headphones, o.kind == Kind::AirPlay, o.name.to_lowercase()));
    out
}

/// Send all sound to this output.
pub fn set_output(o: &Output) -> bool {
    unsafe {
        if let Some(src) = o.source {
            let a = PropertyAddress { selector: DATA_SOURCE, scope: SCOPE_OUTPUT, element: 0 };
            if AudioObjectSetPropertyData(o.device, &a, 0, std::ptr::null(), 4, (&src as *const u32).cast()) != 0 {
                return false;
            }
        }
        let a = addr(DEFAULT_OUTPUT_DEVICE);
        AudioObjectSetPropertyData(SYSTEM_OBJECT, &a, 0, std::ptr::null(), 4, (&o.device as *const u32).cast()) == 0
    }
}

#[cfg(test)]
mod output_tests {
    #[test]
    fn lists_outputs() {
        for o in super::outputs() {
            println!("{o:?}");
        }
    }
}
