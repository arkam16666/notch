//! Real-time audio levels for the equaliser bars, from a Core Audio
//! *process tap* on everything the Mac is playing (macOS 14.2+).
//!
//! Pipeline: CATapDescription (global stereo tap) → process tap →
//! private aggregate device → IO block on the HAL thread. The block splits
//! the signal into 4 bands with one-pole filters (bass / low-mid / high-mid /
//! treble) and publishes their RMS in atomics that the UI reads at 30 fps.
//!
//! First use asks for the "System Audio Recording" permission
//! (NSAudioCaptureUsageDescription in Info.plist).

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, AudioDeviceCreateIOProcIDWithBlock,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};

/// Latest band levels (f32 bits): bass, low-mid, high-mid, treble.
pub static LEVELS: [AtomicU32; 4] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

pub fn levels() -> [f32; 4] {
    std::array::from_fn(|i| f32::from_bits(LEVELS[i].load(Ordering::Relaxed)))
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}

fn ns(c: &std::ffi::CStr) -> Retained<NSString> {
    NSString::from_str(c.to_str().unwrap_or_default())
}

/// UID of the default output device (the aggregate's clock source).
fn default_output_uid() -> Option<Retained<NSString>> {
    unsafe {
        let mut addr = AudioObjectPropertyAddress {
            mSelector: fourcc(b"dOut"),
            mScope: fourcc(b"glob"),
            mElement: 0,
        };
        let mut dev = 0u32;
        let mut size = 4u32;
        if AudioObjectGetPropertyData(1, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut dev).cast()) != 0 {
            return None;
        }
        addr.mSelector = fourcc(b"uid ");
        let mut uid: *mut NSString = std::ptr::null_mut();
        size = std::mem::size_of::<*mut NSString>() as u32;
        if AudioObjectGetPropertyData(dev, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut uid).cast()) != 0 {
            return None;
        }
        Retained::from_raw(uid) // CFString (create rule), toll-free bridged
    }
}

/// One-pole low-pass state for the band split.
struct Bands {
    lp: [f32; 3],
    alpha: [f32; 3],
}

pub struct Tap {
    tap_id: u32,
    agg_id: u32,
    proc_id: AudioDeviceIOProcID,
    _block: RcBlock<dyn Fn(NonNull<AudioTimeStamp>, NonNull<AudioBufferList>, NonNull<AudioTimeStamp>, NonNull<AudioBufferList>, NonNull<AudioTimeStamp>)>,
}

impl Tap {
    /// Start tapping system audio. Err = OSStatus (e.g. permission denied).
    pub fn start() -> Result<Tap, i32> {
        unsafe {
            // 1. global stereo tap (exclude nothing), private, audio keeps playing
            let cls = AnyClass::get(c"CATapDescription").ok_or(-1)?;
            let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
            let none = NSArray::<NSNumber>::new();
            let desc: Option<Retained<AnyObject>> = msg_send![alloc, initStereoGlobalTapButExcludeProcesses: &*none];
            let desc = desc.ok_or(-2)?;
            let _: () = msg_send![&*desc, setPrivate: true];
            let _: () = msg_send![&*desc, setName: &*NSString::from_str("Notch levels")];
            let uuid: Retained<NSUUID> = msg_send![&*desc, UUID];
            let tap_uid = uuid.UUIDString();

            let mut tap_id = 0u32;
            let st = AudioHardwareCreateProcessTap(Some(&*(Retained::as_ptr(&desc) as *const CATapDescription)), &mut tap_id);
            if st != 0 {
                return Err(st);
            }

            // 2. private aggregate device that contains the tap
            let out_uid = default_output_uid().unwrap_or_else(|| NSString::from_str(""));
            let yes = NSNumber::new_bool(true);
            let no = NSNumber::new_bool(false);
            let sub_keys = [&*ns(kAudioSubDeviceUIDKey)];
            let sub_vals: [&AnyObject; 1] = [&out_uid];
            let sub = NSDictionary::from_slices(&sub_keys, &sub_vals);
            let tap_keys = [&*ns(kAudioSubTapDriftCompensationKey), &*ns(kAudioSubTapUIDKey)];
            let tap_vals: [&AnyObject; 2] = [&yes, &tap_uid];
            let tap = NSDictionary::from_slices(&tap_keys, &tap_vals);
            let subs = NSArray::from_retained_slice(&[sub]);
            let taps = NSArray::from_retained_slice(&[tap]);
            let agg_uid = NSUUID::new().UUIDString();
            let name = NSString::from_str("Notch levels");
            let keys = [
                ns(kAudioAggregateDeviceNameKey),
                ns(kAudioAggregateDeviceUIDKey),
                ns(kAudioAggregateDeviceMainSubDeviceKey),
                ns(kAudioAggregateDeviceIsPrivateKey),
                ns(kAudioAggregateDeviceIsStackedKey),
                ns(kAudioAggregateDeviceTapAutoStartKey),
                ns(kAudioAggregateDeviceSubDeviceListKey),
                ns(kAudioAggregateDeviceTapListKey),
            ];
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let vals: [&AnyObject; 8] = [&name, &agg_uid, &out_uid, &yes, &no, &yes, &subs, &taps];
            let dict = NSDictionary::from_slices(&key_refs, &vals);
            let mut agg_id = 0u32;
            let st = AudioHardwareCreateAggregateDevice(
                &*(Retained::as_ptr(&dict) as *const CFDictionary),
                NonNull::from(&mut agg_id),
            );
            if st != 0 {
                AudioHardwareDestroyProcessTap(tap_id);
                return Err(st);
            }

            // 3. IO block: band-split + RMS → LEVELS
            let fs = 48_000.0f32;
            let alpha = |fc: f32| 1.0 - (-2.0 * std::f32::consts::PI * fc / fs).exp();
            let bands = RefCell::new(Bands { lp: [0.0; 3], alpha: [alpha(150.0), alpha(600.0), alpha(2500.0)] });
            let block = RcBlock::new(
                move |_now: NonNull<AudioTimeStamp>,
                      input: NonNull<AudioBufferList>,
                      _t: NonNull<AudioTimeStamp>,
                      _out: NonNull<AudioBufferList>,
                      _t2: NonNull<AudioTimeStamp>| {
                    let list = input.as_ref();
                    let first = list.mBuffers.as_ptr();
                    let mut b = bands.borrow_mut();
                    let mut sum = [0f32; 4];
                    let mut n = 0usize;
                    for bi in 0..list.mNumberBuffers as usize {
                        let buf: &AudioBuffer = &*first.add(bi);
                        if buf.mData.is_null() {
                            continue;
                        }
                        let ch = buf.mNumberChannels.max(1) as usize;
                        let samples = std::slice::from_raw_parts(buf.mData as *const f32, buf.mDataByteSize as usize / 4);
                        for frame in samples.chunks(ch) {
                            let x = frame.iter().sum::<f32>() / ch as f32; // mono mix
                            for k in 0..3 {
                                b.lp[k] += b.alpha[k] * (x - b.lp[k]);
                            }
                            let bands = [b.lp[0], b.lp[1] - b.lp[0], b.lp[2] - b.lp[1], x - b.lp[2]];
                            for k in 0..4 {
                                sum[k] += bands[k] * bands[k];
                            }
                            n += 1;
                        }
                        break; // one buffer is enough (stereo tap = 1 interleaved buffer)
                    }
                    if n > 0 {
                        for k in 0..4 {
                            LEVELS[k].store(((sum[k] / n as f32).sqrt()).to_bits(), Ordering::Relaxed);
                        }
                    }
                },
            );
            let mut proc_id: AudioDeviceIOProcID = None;
            let st = AudioDeviceCreateIOProcIDWithBlock(
                NonNull::from(&mut proc_id),
                agg_id,
                None,
                RcBlock::as_ptr(&block),
            );
            if st != 0 {
                AudioHardwareDestroyAggregateDevice(agg_id);
                AudioHardwareDestroyProcessTap(tap_id);
                return Err(st);
            }
            let st = AudioDeviceStart(agg_id, proc_id);
            if st != 0 {
                AudioDeviceDestroyIOProcID(agg_id, proc_id);
                AudioHardwareDestroyAggregateDevice(agg_id);
                AudioHardwareDestroyProcessTap(tap_id);
                return Err(st);
            }
            Ok(Tap { tap_id, agg_id, proc_id, _block: block })
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.agg_id, self.proc_id);
            AudioDeviceDestroyIOProcID(self.agg_id, self.proc_id);
            AudioHardwareDestroyAggregateDevice(self.agg_id);
            AudioHardwareDestroyProcessTap(self.tap_id);
        }
        for l in &LEVELS {
            l.store(0, Ordering::Relaxed);
        }
    }
}
