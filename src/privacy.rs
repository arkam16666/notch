//! Is a camera in use right now? (green camera icon on the left wing)
//!
//! CoreMediaIO only says whether a camera is running somewhere; macOS doesn't
//! tell third-party apps *which* app is using it.

use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Privacy {
    pub camera: bool,
}

impl Privacy {
    pub fn is_empty(&self) -> bool {
        !self.camera
    }
}

#[repr(C)]
struct Addr {
    selector: u32,
    scope: u32,
    element: u32,
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}
const GLOB: u32 = fourcc(b"glob");

#[link(name = "CoreMediaIO", kind = "framework")]
unsafe extern "C" {
    fn CMIOObjectGetPropertyDataSize(obj: u32, a: *const Addr, qs: u32, q: *const c_void, size: *mut u32) -> i32;
    fn CMIOObjectGetPropertyData(
        obj: u32,
        a: *const Addr,
        qs: u32,
        q: *const c_void,
        size: u32,
        used: *mut u32,
        out: *mut c_void,
    ) -> i32;
}

fn camera_in_use() -> bool {
    let devs_addr = Addr { selector: fourcc(b"dev#"), scope: GLOB, element: 0 };
    let mut size = 0u32;
    unsafe {
        if CMIOObjectGetPropertyDataSize(1, &devs_addr, 0, std::ptr::null(), &mut size) != 0 || size == 0 {
            return false;
        }
        let mut devs = vec![0u32; size as usize / 4];
        let mut used = 0u32;
        if CMIOObjectGetPropertyData(1, &devs_addr, 0, std::ptr::null(), size, &mut used, devs.as_mut_ptr().cast()) != 0 {
            return false;
        }
        devs.truncate(used as usize / 4);
        let running = Addr { selector: fourcc(b"gone"), scope: GLOB, element: 0 };
        devs.into_iter().any(|d| {
            let mut v = 0u32;
            let mut used = 0u32;
            CMIOObjectGetPropertyData(d, &running, 0, std::ptr::null(), 4, &mut used, (&mut v as *mut u32).cast()) == 0
                && v != 0
        })
    }
}

pub fn current() -> Privacy {
    Privacy { camera: camera_in_use() }
}

/// Background loop: poll once a second, send changes.
pub fn run(tx: Sender<Privacy>) {
    let mut last: Option<Privacy> = None;
    loop {
        let p = current();
        if last.as_ref() != Some(&p) {
            if tx.send(p.clone()).is_err() {
                return;
            }
            last = Some(p);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
