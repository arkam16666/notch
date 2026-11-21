mod ask;
mod audio;
mod claude;
mod claude_api;
mod controls;
mod markdown;
mod nets;
mod player;
mod power;
mod privacy;
mod shelf;
mod status;
mod sysinfo;
mod tap;
mod weather;

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, Message, AllocAnyThread, ClassType, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAffineTransformNSAppKitAdditions, NSAnimatablePropertyContainer, NSAnimationContext, NSApplication,
    NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath, NSBitmapImageRep, NSColor,
    NSCompositingOperation, NSDeviceRGBColorSpace, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
    NSFont, NSFontAttributeName, NSFontWeightBold, NSFontWeightMedium, NSFontWeightSemibold,
    NSForegroundColorAttributeName, NSGradient, NSGradientDrawingOptions, NSGraphicsContext,
    NSImage, NSImageSymbolConfiguration, NSLineBreakMode, NSMutableParagraphStyle, NSPanel,
    NSBitmapImageFileType, NSParagraphStyleAttributeName, NSScreen, NSStringDrawing, NSTextAlignment, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSDragOperation, NSDraggingInfo, NSPasteboardReadingOptionKey,
    NSPasteboardTypeFileURL, NSPasteboardURLReadingFileURLsOnlyKey, NSSharingService,
    NSSharingServiceNameSendViaAirDrop, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_graphics::{CGEvent, CGEventTapLocation};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSURL, NSAffineTransform, NSAttributedStringKey, NSData, NSDictionary, NSNumber,
    NSPoint, NSRect, NSSize, NSString, NSTimer,
};
use objc2_quartz_core::CAMediaTimingFunction;
// CLGeocoder is deprecated in favour of MapKit but still works and keeps us
// off yet another framework; it's only used for the city name.
#[allow(deprecated)]
use objc2_core_location::{CLAuthorizationStatus, CLGeocoder, CLLocationManager, CLPlacemark};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession,
    AVCaptureVideoPreviewLayer, AVLayerVideoGravityResizeAspectFill, AVMediaTypeVideo,
};
use objc2_foundation::{NSError, NSObjectProtocol};
use objc2_app_kit::{NSImageInterpolation, NSFocusRingType, NSTextField, NSTextView, NSScrollView,
    NSDraggingItem, NSDraggingSession, NSDraggingSource, NSDraggingContext, NSModalResponse, NSModalResponseOK, NSOpenPanel, NSResponder, NSSecureTextField, NSWindow};

use chrono::Datelike;
use player::{App, Cmd, Msg, Track};

// ---------- layout constants ----------
const EAR: f64 = 8.0; // concave "flare" where the island meets the top edge
const WING: f64 = 42.0; // extra width on each side of the notch when collapsed
const EXP_W: f64 = 650.0;
const EXP_BODY_H: f64 = 144.0; // height below the notch when expanded
const PAD: f64 = 22.0;
/// Body height of the music page (taller: progress + transport + volume rows).
const MUSIC_BODY_H: f64 = 172.0;
const DEFAULT_ACCENT: (f64, f64, f64) = (0.92, 0.92, 0.95);

#[derive(Clone, Copy)]
struct Geometry {
    screen: NSRect,
    notch_w: f64,
    notch_h: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Prev,
    Play,
    Next,
    Tab(Page),
}

/// Pages of the open island, picked from the little menu in the top-left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Music,
    Claude,
    Shelf,
    Weather,
    Camera,
    Controls,
}

const PAGES: [(Page, &str); 6] = [
    (Page::Music, "music.note"),
    (Page::Claude, "sparkle"),
    (Page::Shelf, "tray.full.fill"),
    (Page::Weather, "cloud.sun.fill"),
    (Page::Camera, "camera.fill"),
    (Page::Controls, "switch.2"),
];
/// Claude page: usage bars + the "ask" field; taller once there's an answer.
const CLAUDE_BODY_H: f64 = 188.0;
const CLAUDE_CHAT_H: f64 = 390.0;
/// Ask capsule on the Claude page (inner coords).
fn ask_pill(w: f64, top: f64) -> NSRect {
    rect(PAD, top + 138.0, w - 2.0 * PAD, 36.0)
}
fn ask_send_rect(w: f64, top: f64) -> NSRect {
    let p = ask_pill(w, top);
    rect(p.origin.x + p.size.width - 32.0, p.origin.y + 4.0, 28.0, 28.0)
}
/// File shelf grid.
const SHELF_ITEM_W: f64 = 74.0;

/// Body height of the camera ("mirror") page.
const CAMERA_BODY_H: f64 = 230.0;
const WEATHER_REFRESH_SECS: f64 = 15.0 * 60.0;

/// AVCaptureSession handed to a worker thread (start/stopRunning block).
struct SendSession(Retained<AVCaptureSession>);
unsafe impl Send for SendSession {}
const CC_BLUE: (f64, f64, f64) = (0.04, 0.52, 1.0);
/// Body height when a Wi-Fi / Bluetooth list is open.
const DETAIL_BODY_H: f64 = 244.0;
const ROW_H: f64 = 32.0;

/// In-notch list opened by double-clicking a tile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Detail {
    Wifi,
    Bluetooth,
}

/// Things you can click inside a detail list.
#[derive(Clone, PartialEq, Debug)]
enum Row {
    Net(String),
    Bt(String),
    /// "allow Location to see network names"
    LocationHint,
}

/// The panel only accepts key status while the password field is up, so it
/// never steals typing from the app you're using otherwise.
static WANT_KEY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    struct KeyPanel;

    impl KeyPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            WANT_KEY.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
);
const CHARGE_GREEN: (f64, f64, f64) = (0.3, 0.85, 0.45);
/// Extra width on each side while showing the "charging" banner.
const CHARGE_WING: f64 = 104.0;
const CHARGE_SECS: f64 = 3.2;
const LEVEL_WING: f64 = 92.0;
const LOW_RED: (f64, f64, f64) = (1.0, 0.36, 0.32);
/// Always-on battery readout on the right of the notch.
const BATT_W: f64 = 66.0;

const CAM_GREEN: (f64, f64, f64) = (0.2, 0.84, 0.3);

/// Short-lived banners that bulge out of the collapsed notch.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Banner {
    Charging(u8),
    LowBattery(u8),
    /// volume 0..1, muted
    Volume(f64, bool),
    Brightness(f64),
}

impl Banner {
    /// How long it stays after the last update.
    fn secs(self) -> f64 {
        match self {
            Banner::Charging(_) => CHARGE_SECS,
            Banner::LowBattery(_) => 4.0,
            Banner::Volume(..) | Banner::Brightness(_) => 1.6,
        }
    }
    fn wing(self) -> f64 {
        match self {
            Banner::Charging(_) => CHARGE_WING,
            Banner::LowBattery(_) => CHARGE_WING + 22.0,
            Banner::Volume(..) | Banner::Brightness(_) => LEVEL_WING,
        }
    }
    fn is_level(self) -> bool {
        matches!(self, Banner::Volume(..) | Banner::Brightness(_))
    }
}
/// Fold the wings away this long after music stops / Claude finishes.
const IDLE_SECS: f64 = 5.0;
/// How far the collapsed island bulges out on each side while hovered.
const HOVER_GROW: f64 = 14.0;

/// Things you can grab on the controls page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctl {
    Wifi,
    Bluetooth,
    Dark,
    AirDrop,
    Mute,
    Volume,
    Brightness,
}

struct CtlLayout {
    tiles: [(Ctl, NSRect); 4],
    vol_icon: NSRect,
    vol_track: NSRect,
    bri_icon: NSRect,
    bri_track: NSRect,
}

fn ctl_layout(w: f64, top: f64) -> CtlLayout {
    let gap = 10.0;
    let tw = (w - 2.0 * PAD - 3.0 * gap) / 4.0;
    let tile = |i: usize| rect(PAD + i as f64 * (tw + gap), top + 10.0, tw, 58.0);
    let row = |y: f64| {
        (
            rect(PAD - 2.0, y, 26.0, 24.0),
            rect(PAD + 34.0, y + 9.5, w - 2.0 * PAD - 34.0 - 46.0, 5.0),
        )
    };
    let (vol_icon, vol_track) = row(top + 80.0);
    let (bri_icon, bri_track) = row(top + 110.0);
    CtlLayout {
        tiles: [
            (Ctl::Wifi, tile(0)),
            (Ctl::Bluetooth, tile(1)),
            (Ctl::Dark, tile(2)),
            (Ctl::AirDrop, tile(3)),
        ],
        vol_icon,
        vol_track,
        bri_icon,
        bri_track,
    }
}

/// Do we know (and can we set) the player's own volume?
fn has_player_volume(st: &State) -> bool {
    st.page == Page::Music && st.track.as_ref().is_some_and(|t| t.full && t.volume.is_some())
}

/// Is the song seekable right now (we know position + can command it)?
fn seekable(st: &State) -> bool {
    st.page == Page::Music && st.track.as_ref().is_some_and(|t| t.full && t.duration > 0.0)
}

/// Generous hit box around a slider track.
fn slider_hit(track: NSRect) -> NSRect {
    rect(track.origin.x - 8.0, track.origin.y - 10.0, track.size.width + 16.0, track.size.height + 20.0)
}

fn slider_value(track: NSRect, x: f64) -> f64 {
    ((x - track.origin.x) / track.size.width).clamp(0.0, 1.0)
}
const CLAUDE_ORANGE: (f64, f64, f64) = (0.85, 0.47, 0.34);
/// Claude Code's own spinner glyphs, played forwards then backwards.
const SPINNER: [&str; 10] = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"];

fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

fn fmt_elapsed(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

struct State {
    geo: Geometry,
    page: Page,
    page_changed: Instant,
    usage: Option<claude::Usage>,
    usage_rx: Receiver<claude::Usage>,
    /// Claude Code sessions that are working / waiting / just finished.
    sessions: Vec<status::Session>,
    status_rx: Receiver<Vec<status::Session>>,
    spin_frame: usize,
    controls: Option<controls::Controls>,
    controls_rx: Receiver<controls::Controls>,
    action_tx: Sender<controls::Action>,
    /// last time the user changed a control (ignore stale polls for a bit)
    controls_touched: Option<Instant>,
    dragging: Option<Ctl>,
    detail: Option<Detail>,
    nets: Vec<nets::Net>,
    wifi_err: Option<String>,
    scanning: bool,
    next_scan: Option<Instant>,
    joining: Option<String>,
    join_failed: Option<String>,
    password_for: Option<String>,
    pw_field: Option<Retained<NSSecureTextField>>,
    bt_devs: Vec<nets::BtDev>,
    bt_busy: Option<String>,
    list_scroll: f64,
    req_tx: Sender<nets::Req>,
    resp_rx: Receiver<nets::Resp>,
    location: Option<Retained<CLLocationManager>>,
    // weather
    weather: Option<weather::Weather>,
    weather_err: Option<String>,
    weather_at: Option<Instant>,
    place: Option<String>,
    coords: Option<(f64, f64)>,
    locating: bool,
    wx_tx: Sender<(f64, f64)>,
    wx_rx: Receiver<Result<weather::Weather, String>>,
    #[allow(deprecated)]
    geocoder: Option<Retained<CLGeocoder>>,
    // camera mirror
    cam_session: Option<Retained<AVCaptureSession>>,
    cam_view: Option<Retained<NSView>>,
    cam_on: bool,
    cam_denied: bool,
    cam_asked: bool,
    /// Dragging the player's volume slider (0..1)
    vol_drag: Option<f64>,
    /// Just set the player volume — trust this over stale polls briefly
    vol_target: Option<(f64, Instant)>,
    vol_hover: bool,
    /// Volume before muting via the speaker icon
    vol_before_mute: f64,
    /// Heavily blurred artwork for the music page background
    art_blur: Option<Retained<NSImage>>,
    /// Dragging the song's progress bar: fraction 0..1
    seek_drag: Option<f64>,
    /// Just seeked to (seconds, when) — trust this over stale polls briefly
    seek_target: Option<(f64, Instant)>,
    bar_hover: bool,
    /// clickCount of the mouseDown being handled (2 = double-click)
    click_count: isize,
    /// A tile toggle waiting to see whether a double-click follows.
    pending_tile: Option<(Ctl, Instant)>,
    /// files are being dragged over the island
    drop_hover: bool,
    /// Collapsed island currently shows the side "wings" (vs. just the notch).
    wings: bool,
    // ask Claude
    ask_tx: Sender<ask::Question>,
    ask_rx: Receiver<Result<ask::Answer, String>>,
    ask_session: Option<String>,
    ask_q: Option<String>,
    ask_answer: Option<String>,
    ask_pending: bool,
    ask_field: Option<Retained<NSTextField>>,
    ask_view: Option<Retained<NSScrollView>>,
    // file shelf
    shelf: Vec<std::path::PathBuf>,
    /// while dragging files over the island: true = the shelf half
    drop_to_shelf: bool,
    /// mouse went down on a shelf item (index, where)
    shelf_press: Option<(usize, NSPoint)>,
    shelf_hover: Option<usize>,
    /// file being dragged out of the shelf
    shelf_dragging: Option<std::path::PathBuf>,
    /// Real plan limits from the Claude account (week / 5h %).
    limits: Option<Result<claude_api::Limits, String>>,
    limits_rx: Receiver<Result<claude_api::Limits, String>>,
    limits_poke: Sender<()>,
    limits_poked: Option<Instant>,
    /// CPU / RAM / temperature for the strip next to the battery.
    sys: Option<sysinfo::Stats>,
    sys_rx: Receiver<sysinfo::Stats>,
    /// Live system-audio tap for the equaliser (only while music plays).
    tap: Option<tap::Tap>,
    tap_failed_at: Option<Instant>,
    tap_idle_since: Option<Instant>,
    /// logged once when real audio first arrives (diagnostics)
    tap_heard: bool,
    /// smoothed bar heights and per-band auto-gain peaks
    eq_levels: [f64; 4],
    eq_peak: [f64; 4],
    /// Cursor is over the collapsed island (it bulges out a little).
    hovering: bool,
    /// Apps using the microphone / whether a camera is on.
    privacy: privacy::Privacy,
    privacy_rx: Receiver<privacy::Privacy>,
    /// Last time something was happening (music playing / Claude working).
    last_active: Instant,
    /// "Charging" banner: when it started + battery percent
    /// (banner, shown since, last updated)
    banner: Option<(Banner, Instant, Instant)>,
    on_ac: bool,
    /// last seen output volume / mute, display brightness (for the HUD)
    last_volume: Option<(f32, bool)>,
    last_brightness: Option<f64>,
    hud_poll: u32,
    /// lowest battery level we've already warned about (100 = none)
    low_warned: u8,
    expanded: bool,
    track: Option<Track>,
    track_at: Instant,
    art: Option<Retained<NSImage>>,
    accent: (f64, f64, f64),
    app_icon: Option<(App, Retained<NSImage>)>,
    hover: Option<Btn>,
    pressed: Option<(Btn, Instant)>,
    /// After a play/pause click, show this state until the player catches up
    /// (Chrome keeps its audio running for a moment after pausing).
    optimistic: Option<(bool, Instant)>,
    /// Play-state the button currently shows + when it last changed (morph).
    shown_playing: bool,
    glyph_changed: Option<(bool, Instant)>,
    /// When the island last opened (for click debounce / animation grace).
    expanded_at: Instant,
    symbols: HashMap<String, Retained<NSImage>>,
    started: Instant,
    cmd_tx: Sender<Cmd>,
    msg_rx: Receiver<Msg>,
    window: Option<Retained<NSPanel>>,
}

struct Ivars {
    state: RefCell<State>,
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct NotchView;

    unsafe impl NSObjectProtocol for NotchView {}

    // dragging files *out* of the shelf
    unsafe impl NSDraggingSource for NotchView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_mask(&self, _session: &NSDraggingSession, _ctx: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn drag_ended(&self, _session: &NSDraggingSession, _at: NSPoint, op: NSDragOperation) {
            let done = self.ivars().state.borrow_mut().shelf_dragging.take();
            if let Some(path) = done.filter(|_| op != NSDragOperation::None) {
                // dropped somewhere → it's off the shelf now
                let mut st = self.ivars().state.borrow_mut();
                st.shelf.retain(|p| *p != path);
                shelf::save(&st.shelf);
                drop(st);
                self.setNeedsDisplay(true);
            }
        }
    }

    impl NotchView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.inner_point(event);
            self.ivars().state.borrow_mut().click_count = event.clickCount();
            self.clicked(p);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let p = self.inner_point(event);
            let seeking = self.ivars().state.borrow().seek_drag.is_some();
            let vol = self.ivars().state.borrow().vol_drag.is_some();
            let press = self.ivars().state.borrow().shelf_press;
            if let Some((idx, start)) = press {
                if (p.x - start.x).abs() + (p.y - start.y).abs() > 4.0 {
                    self.ivars().state.borrow_mut().shelf_press = None;
                    self.shelf_drag_out(idx, event);
                }
                return;
            }
            if vol {
                let l = layout(self.inner_width(), self.ivars().state.borrow().geo.notch_h);
                let v = slider_value(l.vol_track, p.x);
                let mut st = self.ivars().state.borrow_mut();
                st.vol_drag = Some(v);
                let _ = st.cmd_tx.send(Cmd::SetVolume(v));
                drop(st);
                self.setNeedsDisplay(true);
            } else if seeking {
                let l = layout(self.inner_width(), self.ivars().state.borrow().geo.notch_h);
                self.ivars().state.borrow_mut().seek_drag = Some(slider_value(l.bar, p.x));
                self.setNeedsDisplay(true);
            } else {
                self.drag_to(p);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            // a click (no drag) on a shelf item opens it
            let press = self.ivars().state.borrow_mut().shelf_press.take();
            if let Some((idx, _)) = press {
                let path = self.ivars().state.borrow().shelf.get(idx).cloned();
                if let Some(path) = path {
                    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                    NSWorkspace::sharedWorkspace().openURL(&url);
                }
                return;
            }
            let mut st = self.ivars().state.borrow_mut();
            st.dragging = None;
            if let Some(v) = st.vol_drag.take() {
                if let Some(t) = st.track.as_mut() {
                    t.volume = Some(v);
                }
                st.vol_target = Some((v, Instant::now()));
            }
            if let Some(frac) = st.seek_drag.take() {
                let dur = st.track.as_ref().map(|t| t.duration).unwrap_or(0.0);
                let secs = frac * dur;
                if let Some(t) = st.track.as_mut() {
                    t.position = secs;
                }
                st.track_at = Instant::now();
                st.seek_target = Some((secs, Instant::now()));
                let _ = st.cmd_tx.send(Cmd::Seek(secs));
                drop(st);
                self.setNeedsDisplay(true);
            }
        }

        // ----- drag files onto the notch → AirDrop -----
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            if dragged_files(sender).is_empty() {
                return NSDragOperation::None;
            }
            if self.ivars().state.borrow().shelf_dragging.is_some() {
                return NSDragOperation::None; // our own drag-out passing over
            }
            let expanded = {
                let mut st = self.ivars().state.borrow_mut();
                st.drop_hover = true;
                st.expanded
            };
            if !expanded {
                self.set_expanded(true);
            }
            self.update_drop_side(sender);
            self.setNeedsDisplay(true);
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            if self.ivars().state.borrow().shelf_dragging.is_some() {
                return NSDragOperation::None;
            }
            self.update_drop_side(sender);
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            self.ivars().state.borrow_mut().drop_hover = false;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            self.ivars().state.borrow_mut().drop_hover = false;
            self.setNeedsDisplay(true);
            let files = dragged_files(sender);
            // (no early `return` here: define_class! rewrites bool returns)
            if files.is_empty() {
                false
            } else if self.ivars().state.borrow().drop_to_shelf {
                self.add_to_shelf(&files);
                true
            } else {
                airdrop(self.mtm(), &files)
            }
        }

        /// Return in the "ask Claude" field.
        #[unsafe(method(askSubmit:))]
        fn ask_submit_action(&self, _sender: Option<&AnyObject>) {
            self.ask_submit();
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            if self.ivars().state.borrow().detail.is_some() {
                let k = if event.hasPreciseScrollingDeltas() { 1.0 } else { 10.0 };
                self.scroll_list(event.scrollingDeltaY() * k);
            }
        }

        /// Return in the password field.
        #[unsafe(method(submitPassword:))]
        fn submit_password_action(&self, _sender: Option<&AnyObject>) {
            self.submit_password();
        }

        /// Esc while typing → cancel.
        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            if self.ivars().state.borrow().password_for.is_some() {
                self.hide_password_field();
                self.ivars().state.borrow_mut().password_for = None;
            } else {
                // Esc in the ask field → start a new conversation
                self.ask_reset();
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            // Right-click = quit
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let p = self.inner_point(event);
            let on_bar = {
                let st = self.ivars().state.borrow();
                st.expanded && seekable(&st) && contains(slider_hit(layout(self.inner_width(), st.geo.notch_h).bar), p)
            };
            let on_vol = {
                let st = self.ivars().state.borrow();
                let l = layout(self.inner_width(), st.geo.notch_h);
                st.expanded && has_player_volume(&st) && (contains(slider_hit(l.vol_track), p) || contains(l.vol_icon, p))
            };
            if on_vol != self.ivars().state.borrow().vol_hover {
                self.ivars().state.borrow_mut().vol_hover = on_vol;
                self.setNeedsDisplay(true);
            }
            if on_bar != self.ivars().state.borrow().bar_hover {
                self.ivars().state.borrow_mut().bar_hover = on_bar;
                self.setNeedsDisplay(true);
            }
            let sh = {
                let st = self.ivars().state.borrow();
                if st.expanded && st.page == Page::Shelf {
                    shelf_rects(self.inner_width(), st.geo.notch_h, st.shelf.len()).iter().position(|r| contains(*r, p))
                } else {
                    None
                }
            };
            if sh != self.ivars().state.borrow().shelf_hover {
                self.ivars().state.borrow_mut().shelf_hover = sh;
                self.setNeedsDisplay(true);
            }
            let hover = self.button_at(p);
            let changed = {
                let mut st = self.ivars().state.borrow_mut();
                let c = st.hover != hover;
                st.hover = hover;
                c
            };
            if changed {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            // Only clear hover here. Collapsing is decided in `tick` from the
            // real cursor position: tracking-area exit events fire spuriously
            // while the window is resizing, and when the cursor is pinned to
            // the very top edge of the screen (exactly on the window's edge).
            self.ivars().state.borrow_mut().hover = None;
            self.setNeedsDisplay(true);
        }
    }
);

// ---------- small helpers ----------
fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn contains(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x
        && p.x <= r.origin.x + r.size.width
        && p.y >= r.origin.y
        && p.y <= r.origin.y + r.size.height
}

fn center(r: NSRect) -> NSPoint {
    NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0)
}

fn white(alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithWhite_alpha(1.0, alpha)
}

fn rgb((r, g, b): (f64, f64, f64), alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, alpha)
}

fn fill_round(r: NSRect, radius: f64, color: &NSColor) {
    color.setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, radius, radius).fill();
}

fn fill_circle(c: NSPoint, radius: f64, color: &NSColor) {
    color.setFill();
    NSBezierPath::bezierPathWithOvalInRect(rect(
        c.x - radius,
        c.y - radius,
        2.0 * radius,
        2.0 * radius,
    ))
    .fill();
}

fn draw_image(img: &NSImage, dst: NSRect, src: NSRect, alpha: f64) {
    unsafe {
        img.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            dst,
            src,
            NSCompositingOperation::SourceOver,
            alpha,
            true,
            None,
        );
    }
}

/// Draw an image centred in `r` at its natural size × `scale`.
fn draw_centered(img: &NSImage, r: NSRect, alpha: f64, scale: f64) {
    let s = img.size();
    let (w, h) = (s.width * scale, s.height * scale);
    let c = center(r);
    draw_image(img, rect(c.x - w / 2.0, c.y - h / 2.0, w, h), NSRect::ZERO, alpha);
}

/// Source rect that crops the image to a centred square ("aspect fill").
fn square_crop(img: &NSImage) -> NSRect {
    let s = img.size();
    let side = s.width.min(s.height);
    rect((s.width - side) / 2.0, (s.height - side) / 2.0, side, side)
}

fn draw_artwork(img: &NSImage, dst: NSRect, radius: f64, alpha: f64) {
    NSGraphicsContext::saveGraphicsState_class();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(dst, radius, radius).addClip();
    draw_image(img, dst, square_crop(img), alpha);
    NSGraphicsContext::restoreGraphicsState_class();
    // hairline border so dark artwork doesn't melt into the black body
    white(0.12 * alpha).setStroke();
    let border = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        rect(
            dst.origin.x + 0.25,
            dst.origin.y + 0.25,
            dst.size.width - 0.5,
            dst.size.height - 0.5,
        ),
        radius,
        radius,
    );
    border.setLineWidth(0.5);
    border.stroke();
}

fn draw_text(text: &str, r: NSRect, font: &NSFont, color: &NSColor, align: NSTextAlignment) {
    let para = NSMutableParagraphStyle::new();
    para.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    para.setAlignment(align);
    unsafe {
        let keys: [&NSAttributedStringKey; 3] = [
            NSFontAttributeName,
            NSForegroundColorAttributeName,
            NSParagraphStyleAttributeName,
        ];
        let vals: [&AnyObject; 3] = [font, color, &para];
        let attrs = NSDictionary::from_slices(&keys, &vals);
        NSString::from_str(text).drawInRect_withAttributes(r, Some(&attrs));
    }
}

fn font(size: f64, weight: f64) -> Retained<NSFont> {
    NSFont::systemFontOfSize_weight(size, weight)
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Soft, Apple-Music-like backdrop: sample the artwork down to 12×12, then
/// rebuild a wide image with smooth (smoothstep-bilinear) interpolation so
/// it reads as a blurred colour field with no visible pixels.
fn blurred(img: &NSImage) -> Option<Retained<NSImage>> {
    const N: usize = 12;
    const W: usize = 192;
    const H: usize = 80;
    let make_rep = |w: usize, h: usize| unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), w as isize, h as isize, 8, 4, true, false,
            NSDeviceRGBColorSpace, (w * 4) as isize, 32,
        )
    };
    // 1. tiny sample
    let small = make_rep(N, N)?;
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&small)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    ctx.setImageInterpolation(NSImageInterpolation::High);
    img.drawInRect_fromRect_operation_fraction(rect(0.0, 0.0, N as f64, N as f64), square_crop(img), NSCompositingOperation::Copy, 1.0);
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    let src = small.bitmapData();
    if src.is_null() {
        return None;
    }
    let src = unsafe { std::slice::from_raw_parts(src, N * N * 4) };

    // 2. smooth upscale into a wide image
    let big = make_rep(W, H)?;
    let dst = big.bitmapData();
    if dst.is_null() {
        return None;
    }
    let dst = unsafe { std::slice::from_raw_parts_mut(dst, W * H * 4) };
    let smooth = |t: f64| t * t * (3.0 - 2.0 * t);
    for y in 0..H {
        // use the middle band of the (square) artwork for the wide backdrop
        let sy = (0.2 + 0.6 * y as f64 / (H - 1) as f64) * (N - 1) as f64;
        let (y0, fy) = (sy.floor() as usize, smooth(sy.fract()));
        let y1 = (y0 + 1).min(N - 1);
        for x in 0..W {
            let sx = x as f64 / (W - 1) as f64 * (N - 1) as f64;
            let (x0, fx) = (sx.floor() as usize, smooth(sx.fract()));
            let x1 = (x0 + 1).min(N - 1);
            for c in 0..4 {
                let p = |xx: usize, yy: usize| src[(yy * N + xx) * 4 + c] as f64;
                let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
                let bot = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
                dst[(y * W + x) * 4 + c] = (top * (1.0 - fy) + bot * fy).round() as u8;
            }
        }
    }
    let out = NSImage::initWithSize(NSImage::alloc(), NSSize::new(W as f64, H as f64));
    out.addRepresentation(&big);
    Some(out)
}

/// Pick a vivid accent colour from the artwork (like Apple Music does).
fn accent_from(img: &NSImage) -> Option<(f64, f64, f64)> {
    const N: isize = 16;
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            N,
            N,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    img.drawInRect_fromRect_operation_fraction(
        rect(0.0, 0.0, N as f64, N as f64),
        square_crop(img),
        NSCompositingOperation::Copy,
        1.0,
    );
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();

    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    let row = rep.bytesPerRow() as usize;
    let (mut sr, mut sg, mut sb, mut sw) = (0.0, 0.0, 0.0, 0.0);
    for y in 0..N as usize {
        for x in 0..N as usize {
            let p = unsafe { std::slice::from_raw_parts(data.add(y * row + x * 4), 4) };
            let (r, g, b) = (p[0] as f64 / 255.0, p[1] as f64 / 255.0, p[2] as f64 / 255.0);
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
            // favour saturated, not-too-dark pixels
            let w = 0.02 + (sat * max).powi(2);
            sr += r * w;
            sg += g * w;
            sb += b * w;
            sw += w;
        }
    }
    let (r, g, b) = (sr / sw, sg / sw, sb / sw);
    let max = r.max(g).max(b).max(1e-3);
    let min = r.min(g).min(b);
    if (max - min) / max < 0.12 {
        return Some(DEFAULT_ACCENT); // greyscale art → soft white accent
    }
    // normalise brightness so the accent always reads on black
    let k = 0.95 / max;
    let lift = |c: f64| (c * k) * 0.85 + 0.15;
    Some((lift(r), lift(g), lift(b)))
}

/// Pick an SF Symbol from a Bluetooth device's name.
fn bt_icon(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("airpods") {
        "airpods"
    } else if n.contains("ipad") {
        "ipad"
    } else if n.contains("iphone") {
        "iphone"
    } else if n.contains("keyboard") || n.contains("คีย์บอร์ด") {
        "keyboard"
    } else if n.contains("mouse") || n.contains("เมาส์") {
        "computermouse"
    } else if n.contains("trackpad") {
        "rectangle.and.hand.point.up.left"
    } else if n.contains("watch") {
        "applewatch"
    } else if n.contains("speaker") || n.contains("homepod") || n.contains("bose") || n.contains("jbl") {
        "hifispeaker"
    } else if n.contains("buds") || n.contains("headphone") || n.contains("wh-") || n.contains("wf-") {
        "headphones"
    } else {
        "dot.radiowaves.left.and.right"
    }
}

/// Width of the mic / camera indicator group on the left wing.
fn privacy_width(p: &privacy::Privacy) -> f64 {
    if p.is_empty() {
        return 0.0;
    }
    if p.camera { 28.0 } else { 0.0 }
}

/// Item rects of the shelf grid (one row; extra items are summarised).
fn shelf_rects(w: f64, top: f64, n: usize) -> Vec<NSRect> {
    let cols = ((w - 2.0 * PAD + 6.0) / (SHELF_ITEM_W + 6.0)).floor().max(1.0) as usize;
    (0..n.min(cols))
        .map(|i| rect(PAD + i as f64 * (SHELF_ITEM_W + 6.0), top + 34.0, SHELF_ITEM_W, 76.0))
        .collect()
}

fn shelf_clear_rect(w: f64, top: f64) -> NSRect {
    rect(w - PAD - 120.0, top + 10.0, 120.0, 16.0)
}

/// System Settings pane opened by double-clicking a tile.
fn settings_pane(ctl: Ctl) -> Option<&'static str> {
    match ctl {
        Ctl::Wifi => Some("x-apple.systempreferences:com.apple.wifi-settings-extension"),
        Ctl::Bluetooth => Some("x-apple.systempreferences:com.apple.BluetoothSettings"),
        Ctl::AirDrop => Some("x-apple.systempreferences:com.apple.AirDrop-Handoff-Settings.extension"),
        _ => None,
    }
}

fn open_url(url: &str) {
    if let Some(u) = NSURL::URLWithString(&NSString::from_str(url)) {
        NSWorkspace::sharedWorkspace().openURL(&u);
    }
}

/// AirDrop button: choose files, then Apple's AirDrop device picker.
fn pick_files_for_airdrop(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.activate(); // accessory app: bring the panel to the front
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(true);
    panel.setMessage(Some(&NSString::from_str("เลือกไฟล์ที่จะส่งผ่าน AirDrop")));
    panel.setPrompt(Some(&NSString::from_str("ส่ง AirDrop")));
    let p = panel.clone();
    let done = RcBlock::new(move |resp: NSModalResponse| {
        if resp == NSModalResponseOK {
            let files: Vec<Retained<NSURL>> = p.URLs().iter().collect();
            if !files.is_empty() {
                airdrop(mtm, &files);
            }
        }
    });
    panel.beginWithCompletionHandler(&done);
}

/// File URLs being dragged.
fn dragged_files(sender: &ProtocolObject<dyn NSDraggingInfo>) -> Vec<Retained<NSURL>> {
    let pb = sender.draggingPasteboard();
    let classes = NSArray::from_slice(&[NSURL::class()]);
    let keys: [&NSPasteboardReadingOptionKey; 1] = [unsafe { NSPasteboardURLReadingFileURLsOnlyKey }];
    let yes = NSNumber::new_bool(true);
    let vals: [&AnyObject; 1] = [&yes];
    let opts = NSDictionary::from_slices(&keys, &vals);
    let Some(items) = (unsafe { pb.readObjectsForClasses_options(&classes, Some(&opts)) }) else {
        return vec![];
    };
    items.iter().filter_map(|o| o.downcast::<NSURL>().ok()).collect()
}

/// Hand files to the system AirDrop picker.
fn airdrop(mtm: MainThreadMarker, files: &[Retained<NSURL>]) -> bool {
    let Some(service) = NSSharingService::sharingServiceNamed(unsafe { NSSharingServiceNameSendViaAirDrop })
    else {
        return false;
    };
    let items: Retained<NSArray> = NSArray::from_retained_slice(
        &files.iter().map(|u| Retained::into_super(Retained::into_super(u.clone()))).collect::<Vec<_>>(),
    );
    if !unsafe { service.canPerformWithItems(Some(&items)) } {
        return false;
    }
    // bring our (accessory) app forward so the AirDrop sheet appears on top
    NSApplication::sharedApplication(mtm).activate();
    unsafe { service.performWithItems(&items) };
    true
}

fn app_icon(app: App) -> Option<Retained<NSImage>> {
    let ws = NSWorkspace::sharedWorkspace();
    let url = ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(app.bundle_id()))?;
    let path = url.path()?;
    Some(ws.iconForFile(&path))
}

// ---------- media keys (fallback control for browser tabs without JS) ----------
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const AnyObject) -> bool;
    static kAXTrustedCheckOptionPrompt: &'static NSString;
}

/// Posting key events needs the Accessibility permission; this shows the
/// system prompt the first time.
fn ensure_accessibility() -> bool {
    unsafe {
        let keys: [&NSString; 1] = [kAXTrustedCheckOptionPrompt];
        let yes = NSNumber::new_bool(true);
        let vals: [&AnyObject; 1] = [&yes];
        let dict = NSDictionary::from_slices(&keys, &vals);
        AXIsProcessTrustedWithOptions(Retained::as_ptr(&dict).cast())
    }
}

/// Simulate a hardware media key (⏯ ⏮ ⏭). macOS routes it to the
/// "Now Playing" app — e.g. Chrome when YouTube was the last thing playing.
fn media_key(cmd: Cmd) {
    const NX_KEYTYPE_PLAY: isize = 16;
    const NX_KEYTYPE_NEXT: isize = 17;
    const NX_KEYTYPE_PREVIOUS: isize = 18;
    let key = match cmd {
        Cmd::PlayPause => NX_KEYTYPE_PLAY,
        Cmd::Next => NX_KEYTYPE_NEXT,
        Cmd::Previous => NX_KEYTYPE_PREVIOUS,
        Cmd::Focus | Cmd::Seek(_) | Cmd::SetVolume(_) => return,
    };
    if !ensure_accessibility() {
        return;
    }
    for flags in [0xa00usize, 0xb00] {
        let ev = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::SystemDefined,
            NSPoint::ZERO,
            NSEventModifierFlags(flags),
            0.0,
            0,
            None,
            8,
            (key << 16) | flags as isize,
            -1,
        );
        if let Some(cg) = ev.and_then(|e| e.CGEvent()) {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&cg));
        }
    }
}

// ---------- layout ----------
struct Layout {
    art: NSRect,
    title: NSRect,
    artist: NSRect,
    eq: NSRect,
    prev: NSRect,
    play: NSRect,
    next: NSRect,
    bar: NSRect,
    vol_icon: NSRect,
    vol_track: NSRect,
    vol_max: NSRect,
}

/// Music page, Apple-Music style:
///
///   ┌──────┐  Title                         ▮▮▮
///   │ art  │  Artist
///   │      │  ━━━━━━━━━━●───────────────────
///   │      │  1:25                      -1:08
///   └──────┘        ⏮     ( ⏸ )     ⏭
///             🔈 ━━━━━━━━━━━━──────────── 🔊
///
/// `w` is the inner width (without the ears), `top` the notch height.
fn layout(w: f64, top: f64) -> Layout {
    let art_s = 124.0;
    let art = rect(PAD, top + 18.0, art_s, art_s);
    let tx = PAD + art_s + 22.0;
    let tw = w - tx - PAD;
    let col_c = tx + tw / 2.0;
    let ctrl_cy = top + 112.0;
    let btn = |cx: f64| rect(cx - 24.0, ctrl_cy - 22.0, 48.0, 44.0);
    let vol_y = top + 148.0;
    Layout {
        art,
        title: rect(tx, top + 20.0, tw - 30.0, 22.0),
        artist: rect(tx, top + 43.0, tw - 30.0, 18.0),
        eq: rect(w - PAD - 18.0, top + 25.0, 18.0, 13.0),
        prev: btn(col_c - 64.0),
        play: btn(col_c),
        next: btn(col_c + 64.0),
        bar: rect(tx, top + 74.0, tw, 4.0),
        vol_icon: rect(tx - 4.0, vol_y - 10.0, 20.0, 20.0),
        vol_track: rect(tx + 22.0, vol_y - 2.0, tw - 46.0, 4.0),
        vol_max: rect(tx + tw - 20.0, vol_y - 10.0, 22.0, 20.0),
    }
}

/// Menu pills in the strip left of the hardware notch (inner coords).
fn tab_rects(notch_h: f64) -> Vec<(Page, NSRect)> {
    let (pw, ph) = (28.0, 22.0_f64.min(notch_h - 6.0));
    let y = (notch_h - ph) / 2.0;
    PAGES
        .iter()
        .enumerate()
        .map(|(i, (page, _))| (*page, rect(PAD - 8.0 + i as f64 * (pw + 3.0), y, pw, ph)))
        .collect()
}

/// Island outline (flipped coords): flat top with concave "ears" flaring
/// into the screen edge, rounded bottom corners.
fn island_path(w: f64, h: f64, radius: f64) -> Retained<NSBezierPath> {
    let p = NSBezierPath::bezierPath();
    let r = radius.min(h / 2.0).min((w - 2.0 * EAR) / 2.0);
    p.moveToPoint(NSPoint::new(0.0, 0.0));
    p.curveToPoint_controlPoint1_controlPoint2(
        NSPoint::new(EAR, EAR),
        NSPoint::new(EAR * 0.6, 0.0),
        NSPoint::new(EAR, EAR * 0.4),
    );
    p.lineToPoint(NSPoint::new(EAR, h - r));
    p.appendBezierPathWithArcFromPoint_toPoint_radius(NSPoint::new(EAR, h), NSPoint::new(EAR + r, h), r);
    p.lineToPoint(NSPoint::new(w - EAR - r, h));
    p.appendBezierPathWithArcFromPoint_toPoint_radius(
        NSPoint::new(w - EAR, h),
        NSPoint::new(w - EAR, h - r),
        r,
    );
    p.lineToPoint(NSPoint::new(w - EAR, EAR));
    p.curveToPoint_controlPoint1_controlPoint2(
        NSPoint::new(w, 0.0),
        NSPoint::new(w - EAR, EAR * 0.4),
        NSPoint::new(w - EAR * 0.6, 0.0),
    );
    p.closePath();
    p
}

/// Little equaliser bars — static (no motion): bright and tall while
/// playing, dim and flat while paused.
/// Equaliser bars. With live `levels` (0..1 per band, from the audio tap)
/// they dance to the music; otherwise a static "playing" / "paused" pattern.
fn draw_eq(r: NSRect, color: (f64, f64, f64), alpha: f64, playing: bool, levels: Option<[f64; 4]>) {
    const BARS: usize = 4;
    let gap = 2.0;
    let bw = (r.size.width - gap * (BARS - 1) as f64) / BARS as f64;
    let col = rgb(color, alpha * if playing { 1.0 } else { 0.4 });
    for i in 0..BARS {
        let fi = i as f64;
        let v = match (playing, levels) {
            (true, Some(l)) => 0.18 + 0.82 * l[i].clamp(0.0, 1.0),
            (true, None) => [0.55, 1.0, 0.7, 0.85][i],
            _ => 0.3,
        };
        let h = (r.size.height * v).max(bw);
        let x = r.origin.x + fi * (bw + gap);
        let y = r.origin.y + (r.size.height - h) / 2.0;
        fill_round(rect(x, y, bw, h), bw / 2.0, &col);
    }
}

/// Snapshot of what `draw` needs, so we don't hold the RefCell while drawing.
struct Frame {
    geo: Geometry,
    page: Page,
    /// 0→1 fade-in after switching pages
    page_t: f64,
    usage: Option<claude::Usage>,
    sessions: Vec<status::Session>,
    spin_frame: usize,
    controls: Option<controls::Controls>,
    seek_drag: Option<f64>,
    bar_hover: bool,
    vol_drag: Option<f64>,
    vol_hover: bool,
    art_blur: Option<Retained<NSImage>>,
    weather: Option<weather::Weather>,
    weather_err: Option<String>,
    place: Option<String>,
    loc_status: Option<CLAuthorizationStatus>,
    cam_on: bool,
    cam_denied: bool,
    detail: Option<Detail>,
    nets: Vec<nets::Net>,
    scanning: bool,
    joining: Option<String>,
    join_failed: Option<String>,
    password_for: Option<String>,
    bt_devs: Vec<nets::BtDev>,
    bt_busy: Option<String>,
    banner: Option<(Banner, Instant, Instant)>,
    privacy: privacy::Privacy,
    sys: Option<sysinfo::Stats>,
    limits: Option<Result<claude_api::Limits, String>>,
    shelf: Vec<std::path::PathBuf>,
    shelf_hover: Option<usize>,
    drop_to_shelf: bool,
    ask_open: bool,
    /// live equaliser heights (None = no audio tap → static bars)
    eq: Option<[f64; 4]>,
    /// (percent, charging)
    battery: Option<(u8, bool)>,
    drop_hover: bool,
    wings: bool,
    track: Option<Track>,
    art: Option<Retained<NSImage>>,
    accent: (f64, f64, f64),
    icon: Option<Retained<NSImage>>,
    hover: Option<Btn>,
    pressed: Option<(Btn, Instant)>,
    glyph_changed: Option<(bool, Instant)>,
    pos: f64,
    dur: f64,
}

impl NotchView {
    fn new(mtm: MainThreadMarker, state: State) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            state: RefCell::new(state),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::MouseMoved
                    | NSTrackingAreaOptions::ActiveAlways
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(&this),
                None,
            )
        };
        this.addTrackingArea(&area);
        this.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));
        this
    }

    /// Mouse position in inner (ear-less) coordinates.
    fn inner_point(&self, event: &NSEvent) -> NSPoint {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        NSPoint::new(p.x - EAR, p.y)
    }

    fn inner_width(&self) -> f64 {
        self.bounds().size.width - 2.0 * EAR
    }

    /// SF Symbol tinted with `color` (cached).
    fn symbol(&self, name: &str, size: f64, color: (f64, f64, f64, f64)) -> Option<Retained<NSImage>> {
        let key = format!("{name}|{size}|{color:?}");
        if let Some(i) = self.ivars().state.borrow().symbols.get(&key) {
            return Some(i.clone());
        }
        let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            None,
        )?;
        let cfg = unsafe {
            NSImageSymbolConfiguration::configurationWithPointSize_weight(size, NSFontWeightBold)
        };
        let col = NSColor::colorWithSRGBRed_green_blue_alpha(color.0, color.1, color.2, color.3);
        let cfg = cfg.configurationByApplyingConfiguration(
            &NSImageSymbolConfiguration::configurationWithHierarchicalColor(&col),
        );
        let img = img.imageWithSymbolConfiguration(&cfg)?;
        self.ivars().state.borrow_mut().symbols.insert(key, img.clone());
        Some(img)
    }

    fn target_frame(&self) -> NSRect {
        let st = self.ivars().state.borrow();
        let g = st.geo;
        let s = g.screen;
        let notch_x = s.origin.x + (s.size.width - g.notch_w) / 2.0;
        if !st.expanded && st.banner.is_none() {
            // collapsed: left = [privacy][music/Claude], right = [eq/Claude]
            let grow = if st.hovering { HOVER_GROW } else { 0.0 };
            let left = if st.wings { WING + privacy_width(&st.privacy) } else { 0.0 } + grow;
            let right = if st.wings { WING } else { 0.0 } + grow;
            let w = g.notch_w + left + right + 2.0 * EAR;
            return rect(notch_x - left - EAR, s.origin.y + s.size.height - g.notch_h, w, g.notch_h);
        }
        let (w, h) = if st.expanded {
            let body = match st.page {
                Page::Controls if st.detail.is_some() => DETAIL_BODY_H,
                Page::Camera => CAMERA_BODY_H,
                Page::Music => MUSIC_BODY_H,
                Page::Claude if st.ask_answer.is_some() || st.ask_pending => CLAUDE_CHAT_H,
                Page::Claude => CLAUDE_BODY_H,
                _ => EXP_BODY_H,
            };
            (EXP_W.max(g.notch_w + 2.0 * WING), g.notch_h + body)
        } else {
            let b = st.banner.map(|(b, ..)| b.wing()).unwrap_or(WING);
            (g.notch_w + 2.0 * b, g.notch_h)
        };
        let w = w + 2.0 * EAR;
        rect(
            s.origin.x + (s.size.width - w) / 2.0,
            s.origin.y + s.size.height - h,
            w,
            h,
        )
    }

    fn relayout(&self) {
        let frame = self.target_frame();
        let win = self.ivars().state.borrow().window.clone();
        let Some(win) = win else { return };
        let cur = win.frame();
        let expanding = frame.size.width > cur.size.width + 0.5 || frame.size.height > cur.size.height + 0.5;
        let block = RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
            let ctx = unsafe { ctx.as_ref() };
            // Springy overshoot when growing (open / hover bulge), smooth ease
            // when shrinking.
            let big = frame.size.height > cur.size.height + 0.5;
            let (dur, f) = if expanding && big {
                (0.45, CAMediaTimingFunction::functionWithControlPoints(0.3, 1.35, 0.5, 1.0))
            } else if expanding {
                // hover bulge: quicker, bouncier
                (0.38, CAMediaTimingFunction::functionWithControlPoints(0.25, 1.8, 0.45, 1.0))
            } else {
                (0.3, CAMediaTimingFunction::functionWithControlPoints(0.4, 0.0, 0.2, 1.0))
            };
            ctx.setDuration(dur);
            ctx.setTimingFunction(Some(&f));
            win.animator().setFrame_display(frame, true);
        });
        NSAnimationContext::runAnimationGroup(&block);
    }

    fn set_expanded(&self, e: bool) {
        {
            let mut st = self.ivars().state.borrow_mut();
            if e && !st.expanded {
                st.expanded_at = Instant::now();
            }
            st.expanded = e;
            if !e {
                st.detail = None;
            }
        }
        if !e {
            self.hide_password_field();
        }
        self.relayout();
    }

    fn position_now(st: &State) -> (f64, f64) {
        match &st.track {
            Some(t) => {
                let mut p = t.position;
                if t.playing {
                    p += st.track_at.elapsed().as_secs_f64();
                }
                (p.min(t.duration), t.duration)
            }
            None => (0.0, 0.0),
        }
    }

    fn button_at(&self, p: NSPoint) -> Option<Btn> {
        let st = self.ivars().state.borrow();
        if !st.expanded {
            return None;
        }
        if let Some((page, _)) = tab_rects(st.geo.notch_h).into_iter().find(|(_, r)| contains(*r, p)) {
            return Some(Btn::Tab(page));
        }
        if st.page != Page::Music || st.track.is_none() {
            return None;
        }
        let l = layout(self.inner_width(), st.geo.notch_h);
        [(l.prev, Btn::Prev), (l.play, Btn::Play), (l.next, Btn::Next)]
            .into_iter()
            .find(|(r, _)| contains(*r, p))
            .map(|(_, b)| b)
    }

    fn clicked(&self, p: NSPoint) {
        let (expanded, notch_h, track, opened_for) = {
            let st = self.ivars().state.borrow();
            (st.expanded, st.geo.notch_h, st.track.clone(), st.expanded_at.elapsed().as_secs_f64())
        };
        if !expanded {
            self.set_expanded(true);
            return;
        }
        // Ignore the 2nd click of a double-click (or a click landing while the
        // open animation is still running) so it doesn't close right away.
        if opened_for < 0.4 {
            return;
        }
        {
            let mut st = self.ivars().state.borrow_mut();
            let l = layout(self.inner_width(), notch_h);
            if seekable(&st) && contains(slider_hit(l.bar), p) {
                st.seek_drag = Some(slider_value(l.bar, p.x));
                drop(st);
                return self.setNeedsDisplay(true);
            }
            if has_player_volume(&st) {
                if contains(slider_hit(l.vol_track), p) {
                    let v = slider_value(l.vol_track, p.x);
                    st.vol_drag = Some(v);
                    let _ = st.cmd_tx.send(Cmd::SetVolume(v));
                    drop(st);
                    return self.setNeedsDisplay(true);
                }
                if contains(l.vol_icon, p) {
                    // speaker icon: mute ↔ previous level
                    let cur = st.track.as_ref().and_then(|t| t.volume).unwrap_or(0.0);
                    let v = if cur > 0.0 {
                        st.vol_before_mute = cur;
                        0.0
                    } else {
                        st.vol_before_mute.max(0.1)
                    };
                    if let Some(t) = st.track.as_mut() {
                        t.volume = Some(v);
                    }
                    st.vol_target = Some((v, Instant::now()));
                    let _ = st.cmd_tx.send(Cmd::SetVolume(v));
                    drop(st);
                    return self.setNeedsDisplay(true);
                }
            }
        }
        let page = self.ivars().state.borrow().page;
        if page == Page::Claude && contains(ask_send_rect(self.inner_width(), notch_h), p) {
            self.ask_submit();
            return;
        }
        if page == Page::Shelf && p.y > notch_h {
            self.shelf_click(p);
            return;
        }
        if page == Page::Weather && p.y > notch_h {
            let denied = self.ivars().state.borrow().location.as_ref().is_some_and(|l| {
                let s = unsafe { l.authorizationStatus() };
                s == CLAuthorizationStatus::Denied || s == CLAuthorizationStatus::Restricted
            });
            if denied {
                open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices");
                self.set_expanded(false);
            }
            return;
        }
        if page == Page::Camera && p.y > notch_h {
            if self.ivars().state.borrow().cam_denied {
                open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Camera");
                self.set_expanded(false);
            }
            return;
        }
        let on_controls = self.ivars().state.borrow().page == Page::Controls;
        if on_controls && p.y > notch_h {
            self.controls_click(p);
            return;
        }
        if let Some(btn) = self.button_at(p) {
            let cmd = match btn {
                Btn::Prev => Cmd::Previous,
                Btn::Play => Cmd::PlayPause,
                Btn::Next => Cmd::Next,
                Btn::Tab(page) => {
                    let mut st = self.ivars().state.borrow_mut();
                    let changed = st.page != page;
                    if changed {
                        st.page = page;
                        st.page_changed = Instant::now();
                    }
                    drop(st);
                    if changed {
                        // pages have different heights (camera, Wi-Fi list…)
                        self.relayout();
                    }
                    if page == Page::Claude {
                        let mut st = self.ivars().state.borrow_mut();
                        if st.limits_poked.is_none_or(|t| t.elapsed().as_secs_f64() > 60.0) {
                            st.limits_poked = Some(Instant::now());
                            let _ = st.limits_poke.send(());
                        }
                    }
                    self.setNeedsDisplay(true);
                    return;
                }
            };
            {
                let mut st = self.ivars().state.borrow_mut();
                st.pressed = Some((btn, Instant::now()));
                // Optimistic UI update for play/pause.
                if cmd == Cmd::PlayPause {
                    let (pos, _) = Self::position_now(&st);
                    let mut new_state = None;
                    if let Some(t) = st.track.as_mut() {
                        t.playing = !t.playing;
                        t.position = pos;
                        new_state = Some(t.playing);
                    }
                    st.optimistic = new_state.map(|p| (p, Instant::now()));
                    st.track_at = Instant::now();
                }
            }
            match &track {
                Some(t) if !t.full => media_key(cmd),
                _ => {
                    let _ = self.ivars().state.borrow().cmd_tx.send(cmd);
                }
            }
            self.setNeedsDisplay(true);
            return;
        }
        let l = layout(self.inner_width(), notch_h);
        let on_music = self.ivars().state.borrow().page == Page::Music;
        if on_music && (contains(l.art, p) || contains(l.title, p) || contains(l.artist, p)) {
            // Click artwork / title → bring the player app / tab to the front.
            if track.is_some() {
                let _ = self.ivars().state.borrow().cmd_tx.send(Cmd::Focus);
            }
        } else if p.y <= notch_h {
            // Clicking the notch strip again closes it; empty space does nothing.
            self.set_expanded(false);
        }
    }

    fn ctl_at(&self, p: NSPoint) -> Option<Ctl> {
        let l = ctl_layout(self.inner_width(), self.ivars().state.borrow().geo.notch_h);
        if let Some((c, _)) = l.tiles.iter().find(|(_, r)| contains(*r, p)) {
            return Some(*c);
        }
        if contains(l.vol_icon, p) {
            Some(Ctl::Mute)
        } else if contains(slider_hit(l.vol_track), p) {
            Some(Ctl::Volume)
        } else if contains(slider_hit(l.bri_track), p) {
            Some(Ctl::Brightness)
        } else {
            None
        }
    }

    fn controls_click(&self, p: NSPoint) {
        if self.ivars().state.borrow().detail.is_some() {
            return self.detail_click(p);
        }
        let Some(ctl) = self.ctl_at(p) else { return };
        if let Some(pane) = settings_pane(ctl) {
            let mut st = self.ivars().state.borrow_mut();
            if st.click_count >= 2 {
                // double-click: cancel the pending toggle and open the list
                // right here in the notch (AirDrop → its Settings pane)
                st.pending_tile = None;
                drop(st);
                match ctl {
                    Ctl::Wifi => self.open_detail(Detail::Wifi),
                    Ctl::Bluetooth => self.open_detail(Detail::Bluetooth),
                    _ => {
                        open_url(pane);
                        self.set_expanded(false);
                    }
                }
            } else {
                let wait = NSEvent::doubleClickInterval();
                st.pending_tile = Some((ctl, Instant::now() + std::time::Duration::from_secs_f64(wait)));
                st.pressed = Some((Btn::Tab(Page::Controls), Instant::now()));
            }
            return;
        }
        self.apply_tile(ctl, p);
    }

    /// View-space rect around the equaliser bars (collapsed wing or the
    /// music page), generously padded.
    fn eq_dirty_rect(&self) -> NSRect {
        let st = self.ivars().state.borrow();
        let b = self.bounds();
        if st.expanded {
            let l = layout(self.inner_width(), st.geo.notch_h);
            rect(l.eq.origin.x + EAR - 4.0, l.eq.origin.y - 4.0, l.eq.size.width + 8.0, l.eq.size.height + 8.0)
        } else {
            rect(b.size.width - EAR - WING - 8.0, 0.0, WING + 8.0, b.size.height)
        }
    }

    /// Run the audio tap while music plays and the bars are visible; turn
    /// band levels into smooth, auto-gained bar heights. Returns "redraw".
    fn tick_eq(&self) -> bool {
        let mut st = self.ivars().state.borrow_mut();
        let playing = st.track.as_ref().is_some_and(|t| t.playing);
        let visible = st.wings || (st.expanded && st.page == Page::Music);
        let want = playing && visible;
        if want {
            st.tap_idle_since = None;
            let may_retry = st.tap_failed_at.is_none_or(|t| t.elapsed().as_secs_f64() > 30.0);
            if st.tap.is_none() && may_retry {
                match tap::Tap::start() {
                    Ok(t) => {
                        eprintln!("notch: audio tap started");
                        st.tap = Some(t);
                        st.tap_failed_at = None;
                    }
                    Err(e) => {
                        eprintln!("notch: audio tap failed: OSStatus {e}");
                        st.tap_failed_at = Some(Instant::now());
                    }
                }
            }
        } else if st.tap.is_some() {
            // keep it a moment (pause / skip), then release the tap
            let since = *st.tap_idle_since.get_or_insert_with(Instant::now);
            if since.elapsed().as_secs_f64() > 3.0 {
                st.tap = None;
                st.tap_idle_since = None;
                st.eq_levels = [0.0; 4];
            }
        }
        if st.tap.is_none() || !playing {
            return false;
        }
        let raw = tap::levels();
        if raw.iter().any(|v| *v > 0.0) && !st.tap_heard {
            st.tap_heard = true;
            eprintln!("notch: audio tap receiving audio (bands {raw:?})");
        }
        for k in 0..4 {
            let lv = raw[k] as f64;
            // auto-gain: track each band's recent peak, decaying slowly
            st.eq_peak[k] = lv.max(st.eq_peak[k] * 0.992).max(1e-4);
            let norm = (lv / st.eq_peak[k]).clamp(0.0, 1.0).sqrt();
            // fast attack, slower fall
            let cur = st.eq_levels[k];
            let k_s = if norm > cur { 0.65 } else { 0.22 };
            st.eq_levels[k] = cur + (norm - cur) * k_s;
        }
        true
    }

    /// Weather page: get permission → location → (city name) → forecast.
    #[allow(deprecated)]
    fn tick_weather(&self) {
        let mut st = self.ivars().state.borrow_mut();
        while let Ok(r) = st.wx_rx.try_recv() {
            match r {
                Ok(w) => {
                    st.weather = Some(w);
                    st.weather_err = None;
                }
                Err(e) => st.weather_err = Some(e),
            }
            drop(st);
            self.setNeedsDisplay(true);
            st = self.ivars().state.borrow_mut();
        }
        if !(st.expanded && st.page == Page::Weather) {
            return;
        }
        let Some(lm) = st.location.clone() else { return };
        let status = unsafe { lm.authorizationStatus() };
        if status == CLAuthorizationStatus::NotDetermined {
            if !st.locating {
                unsafe { lm.requestWhenInUseAuthorization() };
                st.locating = true; // don't spam the request
            }
            return;
        }
        if status != CLAuthorizationStatus::AuthorizedAlways && status != CLAuthorizationStatus::AuthorizedWhenInUse {
            return;
        }
        if st.coords.is_none() {
            if !st.locating || st.weather_at.is_none() {
                unsafe { lm.startUpdatingLocation() };
                st.locating = true;
            }
            if let Some(loc) = unsafe { lm.location() } {
                let c: objc2_core_location::CLLocationCoordinate2D = unsafe { msg_send![&*loc, coordinate] };
                st.coords = Some((c.latitude, c.longitude));
                unsafe { lm.stopUpdatingLocation() };
                st.locating = false;
                // city name (Apple's geocoder, on-device service)
                let geocoder = st.geocoder.get_or_insert_with(|| unsafe { CLGeocoder::new() }).clone();
                let view = self.retain();
                let done = RcBlock::new(move |marks: *mut NSArray<CLPlacemark>, _err: *mut NSError| {
                    let Some(marks) = (unsafe { marks.as_ref() }) else { return };
                    let name = marks.iter().next().and_then(|p| unsafe { p.locality() }).map(|s| s.to_string());
                    view.ivars().state.borrow_mut().place = name;
                    view.setNeedsDisplay(true);
                });
                unsafe { geocoder.reverseGeocodeLocation_completionHandler(&loc, RcBlock::as_ptr(&done)) };
            }
        }
        if let Some((lat, lon)) = st.coords {
            if st.weather_at.is_none_or(|t| t.elapsed().as_secs_f64() > WEATHER_REFRESH_SECS) {
                st.weather_at = Some(Instant::now());
                let _ = st.wx_tx.send((lat, lon));
            }
        }
    }

    /// Camera page: run the front camera only while the page is visible.
    fn tick_camera(&self) {
        let want = {
            let st = self.ivars().state.borrow();
            st.expanded && st.page == Page::Camera && st.expanded_at.elapsed().as_secs_f64() > 0.15
        };
        let on = self.ivars().state.borrow().cam_on;
        if want && !on {
            self.start_camera();
        } else if !want && on {
            self.stop_camera();
        }
    }

    fn camera_rect(&self) -> NSRect {
        let top = self.ivars().state.borrow().geo.notch_h;
        rect(PAD - 6.0, top + 8.0, self.inner_width() - 2.0 * PAD + 12.0, CAMERA_BODY_H - 18.0)
    }

    fn start_camera(&self) {
        let Some(video) = (unsafe { AVMediaTypeVideo }) else { return };
        match unsafe { AVCaptureDevice::authorizationStatusForMediaType(video) } {
            AVAuthorizationStatus::NotDetermined => {
                // ask once; later ticks re-check and start as soon as it's granted
                let mut st = self.ivars().state.borrow_mut();
                if !st.cam_asked {
                    st.cam_asked = true;
                    let asked = RcBlock::new(|_ok: objc2::runtime::Bool| {});
                    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(video, &asked) };
                }
                return;
            }
            AVAuthorizationStatus::Authorized => {}
            _ => {
                let mut st = self.ivars().state.borrow_mut();
                st.cam_denied = true;
                st.cam_on = true;
                return;
            }
        }
        let mut st = self.ivars().state.borrow_mut();
        st.cam_denied = false;
        if st.cam_session.is_none() {
            let Some(device) = (unsafe { AVCaptureDevice::defaultDeviceWithMediaType(video) }) else { return };
            let Ok(input) = (unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }) else { return };
            let session = unsafe { AVCaptureSession::new() };
            unsafe {
                if session.canAddInput(&input) {
                    session.addInput(&input);
                }
            }
            st.cam_session = Some(session);
        }
        let session = st.cam_session.clone().unwrap();
        drop(st);

        // layer-backed host view with the live preview, mirrored like a mirror
        let r = self.camera_rect();
        let host = NSView::new(self.mtm());
        host.setFrame(rect(r.origin.x + EAR, r.origin.y, r.size.width, r.size.height));
        host.setWantsLayer(true);
        if let Some(layer) = host.layer() {
            layer.setCornerRadius(16.0);
            layer.setMasksToBounds(true);
            let preview = unsafe { AVCaptureVideoPreviewLayer::layerWithSession(&session) };
            if let Some(g) = unsafe { AVLayerVideoGravityResizeAspectFill } {
                unsafe { preview.setVideoGravity(g) };
            }
            if let Some(conn) = unsafe { preview.connection() } {
                unsafe {
                    conn.setAutomaticallyAdjustsVideoMirroring(false);
                    conn.setVideoMirrored(true);
                }
            }
            preview.setFrame(rect(0.0, 0.0, r.size.width, r.size.height));
            layer.addSublayer(&preview);
        }
        self.addSubview(&host);
        let worker = SendSession(session);
        std::thread::spawn(move || { let w = worker; unsafe { w.0.startRunning() } });
        let mut st = self.ivars().state.borrow_mut();
        st.cam_view = Some(host);
        st.cam_on = true;
    }

    fn stop_camera(&self) {
        let mut st = self.ivars().state.borrow_mut();
        st.cam_on = false;
        if let Some(v) = st.cam_view.take() {
            v.removeFromSuperview();
        }
        if let Some(s) = st.cam_session.clone() {
            let worker = SendSession(s);
            std::thread::spawn(move || { let w = worker; unsafe { w.0.stopRunning() } });
        }
    }

    // ================= ask Claude =================

    /// Keep the text field / answer view in sync with the Claude page.
    fn tick_ask(&self) {
        let (want, top, has_answer) = {
            let st = self.ivars().state.borrow();
            (
                st.expanded && st.page == Page::Claude && st.expanded_at.elapsed().as_secs_f64() > 0.2,
                st.geo.notch_h,
                st.ask_answer.is_some() || st.ask_pending,
            )
        };
        let w = self.inner_width();
        let mtm = self.mtm();
        if !want {
            let (f, v) = {
                let mut st = self.ivars().state.borrow_mut();
                (st.ask_field.take(), st.ask_view.take())
            };
            if let Some(f) = f {
                f.removeFromSuperview();
            }
            if let Some(v) = v {
                v.removeFromSuperview();
            }
            if !self.ivars().state.borrow().pw_field.is_some() {
                WANT_KEY.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            return;
        }
        if self.ivars().state.borrow().ask_field.is_none() {
            let pill = ask_pill(w, top);
            let field = NSTextField::new(mtm);
            // sits inside the capsule we draw: after the ✦, before the send button
            field.setFrame(rect(pill.origin.x + 36.0 + EAR, pill.origin.y + 8.0, pill.size.width - 36.0 - 42.0, 20.0));
            field.setBezeled(false);
            field.setBordered(false);
            field.setDrawsBackground(false);
            field.setTextColor(Some(&NSColor::whiteColor()));
            field.setFont(Some(&font(14.0, 0.0)));
            field.setFocusRingType(NSFocusRingType::None);
            unsafe {
                let ph_font = font(14.0, 0.0);
                let ph_col = NSColor::colorWithWhite_alpha(1.0, 0.35);
                let keys: [&NSAttributedStringKey; 2] = [NSFontAttributeName, NSForegroundColorAttributeName];
                let vals: [&AnyObject; 2] = [&ph_font, &ph_col];
                let attrs = NSDictionary::from_slices(&keys, &vals);
                let ph = objc2_foundation::NSAttributedString::initWithString_attributes(
                    objc2_foundation::NSAttributedString::alloc(),
                    &NSString::from_str("ถาม Claude…"),
                    Some(&attrs),
                );
                field.setPlaceholderAttributedString(Some(&ph));
            }
            unsafe {
                field.setTarget(Some(self));
                field.setAction(Some(sel!(askSubmit:)));
            }
            self.addSubview(&field);
            WANT_KEY.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(win) = self.window() {
                win.makeKeyWindow();
                win.makeFirstResponder(Some(&field));
            }
            self.ivars().state.borrow_mut().ask_field = Some(field);
        }
        let need_view = has_answer && self.bounds().size.height >= top + CLAUDE_CHAT_H - 2.0;
        let have_view = self.ivars().state.borrow().ask_view.is_some();
        if need_view && !have_view {
            let scroll = NSTextView::scrollableTextView(mtm);
            scroll.setFrame(rect(PAD + EAR + 4.0, top + 190.0, w - 2.0 * PAD - 8.0, CLAUDE_CHAT_H - 190.0 - 18.0));
            scroll.setDrawsBackground(false);
            scroll.setAutohidesScrollers(true);
            if let Some(tv) = scroll.documentView().and_then(|v| v.downcast::<NSTextView>().ok()) {
                tv.setEditable(false);
                tv.setSelectable(true);
                tv.setDrawsBackground(false);
                tv.setTextContainerInset(NSSize::new(10.0, 10.0));
            }
            self.addSubview(&scroll);
            self.ivars().state.borrow_mut().ask_view = Some(scroll);
            self.refresh_answer();
        } else if !need_view && have_view {
            let v = self.ivars().state.borrow_mut().ask_view.take();
            if let Some(v) = v {
                v.removeFromSuperview();
            }
        }
    }

    /// Put the current Q/A into the answer view.
    fn refresh_answer(&self) {
        let st = self.ivars().state.borrow();
        let Some(scroll) = &st.ask_view else { return };
        let Some(tv) = scroll.documentView().and_then(|v| v.downcast::<NSTextView>().ok()) else { return };
        let q = st.ask_q.clone().unwrap_or_default();
        let answer = if st.ask_pending { "✻ กำลังคิด…".to_string() } else { st.ask_answer.clone().unwrap_or_default() };
        let rendered = markdown::render(&q, &answer, &markdown::Theme { size: 13.5 });
        if let Some(storage) = unsafe { tv.textStorage() } {
            storage.setAttributedString(&rendered);
        }
        tv.scrollRangeToVisible(objc2_foundation::NSRange::new(0, 0));
    }

    fn ask_submit(&self) {
        let text = {
            let st = self.ivars().state.borrow();
            st.ask_field.as_ref().map(|f| f.stringValue().to_string()).unwrap_or_default()
        };
        let text = text.trim().to_string();
        if text.is_empty() || self.ivars().state.borrow().ask_pending {
            return;
        }
        {
            let mut st = self.ivars().state.borrow_mut();
            if let Some(f) = &st.ask_field {
                f.setStringValue(&NSString::from_str(""));
            }
            st.ask_pending = true;
            st.ask_q = Some(text.clone());
            let session = st.ask_session.clone();
            let _ = st.ask_tx.send(ask::Question { text, session });
        }
        self.refresh_answer();
        self.relayout();
        self.setNeedsDisplay(true);
    }

    fn ask_reset(&self) {
        {
            let mut st = self.ivars().state.borrow_mut();
            if st.ask_pending {
                return; // let the running answer finish
            }
            st.ask_session = None;
            st.ask_q = None;
            st.ask_answer = None;
        }
        self.relayout();
    }

    // ================= file shelf =================

    fn update_drop_side(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) {
        let p = self.convertPoint_fromView(sender.draggingLocation(), None);
        let left = p.x - EAR < self.inner_width() / 2.0;
        let mut st = self.ivars().state.borrow_mut();
        if st.drop_to_shelf != left {
            st.drop_to_shelf = left;
            drop(st);
            self.setNeedsDisplay(true);
        }
    }

    fn add_to_shelf(&self, files: &[Retained<NSURL>]) {
        {
            let mut st = self.ivars().state.borrow_mut();
            for u in files {
                if let Some(path) = u.path() {
                    let pb = std::path::PathBuf::from(path.to_string());
                    if !st.shelf.contains(&pb) {
                        st.shelf.push(pb);
                    }
                }
            }
            shelf::save(&st.shelf);
            if st.page != Page::Shelf {
                st.page = Page::Shelf;
                st.page_changed = Instant::now();
            }
        }
        self.relayout();
        self.setNeedsDisplay(true);
    }

    fn shelf_click(&self, p: NSPoint) {
        let w = self.inner_width();
        let (top, n) = {
            let st = self.ivars().state.borrow();
            (st.geo.notch_h, st.shelf.len())
        };
        if n > 0 && contains(shelf_clear_rect(w, top), p) {
            let mut st = self.ivars().state.borrow_mut();
            st.shelf.clear();
            shelf::save(&st.shelf);
            drop(st);
            return self.setNeedsDisplay(true);
        }
        let rects = shelf_rects(w, top, n);
        let Some(i) = rects.iter().position(|r| contains(*r, p)) else { return };
        // little × in the corner removes it
        let r = rects[i];
        if contains(rect(r.origin.x + r.size.width - 22.0, r.origin.y, 22.0, 22.0), p) {
            let mut st = self.ivars().state.borrow_mut();
            st.shelf.remove(i);
            st.shelf_hover = None;
            shelf::save(&st.shelf);
            drop(st);
            return self.setNeedsDisplay(true);
        }
        self.ivars().state.borrow_mut().shelf_press = Some((i, p));
    }

    fn shelf_drag_out(&self, idx: usize, event: &NSEvent) {
        let (path, top) = {
            let st = self.ivars().state.borrow();
            (st.shelf.get(idx).cloned(), st.geo.notch_h)
        };
        let Some(path) = path else { return };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
        let r = shelf_rects(self.inner_width(), top, idx + 1)[idx];
        let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&path.to_string_lossy()));
        unsafe {
            item.setDraggingFrame_contents(
                rect(r.origin.x + EAR + (r.size.width - 44.0) / 2.0, r.origin.y + 6.0, 44.0, 44.0),
                Some(&icon),
            )
        };
        self.ivars().state.borrow_mut().shelf_dragging = Some(path);
        let items = NSArray::from_retained_slice(&[item]);
        self.beginDraggingSessionWithItems_event_source(&items, event, ProtocolObject::from_ref(self));
    }

    fn open_detail(&self, d: Detail) {
        {
            let mut st = self.ivars().state.borrow_mut();
            st.detail = Some(d);
            st.list_scroll = 0.0;
            st.join_failed = None;
            match d {
                Detail::Wifi => {
                    // network names need Location permission (asked once)
                    if let Some(lm) = &st.location {
                        if unsafe { lm.authorizationStatus() } == CLAuthorizationStatus::NotDetermined {
                            unsafe { lm.requestWhenInUseAuthorization() };
                        }
                    }
                    st.scanning = true;
                    let _ = st.req_tx.send(nets::Req::ScanWifi);
                    st.next_scan = Some(Instant::now() + std::time::Duration::from_secs(12));
                }
                Detail::Bluetooth => {
                    let _ = st.req_tx.send(nets::Req::ListBt);
                }
            }
        }
        self.relayout();
        self.setNeedsDisplay(true);
    }

    fn close_detail(&self) {
        self.hide_password_field();
        {
            let mut st = self.ivars().state.borrow_mut();
            st.detail = None;
            st.password_for = None;
        }
        self.relayout();
        self.setNeedsDisplay(true);
    }

    /// Rows of the open list with their rects (inner coords, before clipping).
    fn detail_rows(&self, w: f64) -> Vec<(Row, NSRect)> {
        let st = self.ivars().state.borrow();
        let top = st.geo.notch_h;
        let mut y = top + 46.0 - st.list_scroll;
        let x = PAD - 6.0;
        let rw = w - 2.0 * PAD + 12.0;
        let mut out = Vec::new();
        match st.detail {
            Some(Detail::Wifi) => {
                if st.wifi_err.as_deref() == Some("location") {
                    out.push((Row::LocationHint, rect(x, y, rw, 52.0)));
                    return out;
                }
                for n in &st.nets {
                    let h = if st.password_for.as_deref() == Some(&n.ssid) { ROW_H + 38.0 } else { ROW_H };
                    out.push((Row::Net(n.ssid.clone()), rect(x, y, rw, h - 2.0)));
                    y += h;
                }
            }
            Some(Detail::Bluetooth) => {
                for d in &st.bt_devs {
                    out.push((Row::Bt(d.addr.clone()), rect(x, y, rw, ROW_H - 2.0)));
                    y += ROW_H;
                }
            }
            None => {}
        }
        out
    }

    fn list_area(&self) -> NSRect {
        let st = self.ivars().state.borrow();
        let top = st.geo.notch_h;
        rect(0.0, top + 42.0, self.inner_width(), DETAIL_BODY_H - 50.0)
    }

    fn detail_header(&self, w: f64) -> (NSRect, NSRect) {
        let top = self.ivars().state.borrow().geo.notch_h;
        (rect(PAD - 8.0, top + 6.0, 110.0, 30.0), rect(w - PAD - 40.0, top + 10.0, 40.0, 22.0))
    }

    fn detail_click(&self, p: NSPoint) {
        let w = self.inner_width();
        let (back, switch) = self.detail_header(w);
        let detail = self.ivars().state.borrow().detail;
        if contains(back, p) {
            return self.close_detail();
        }
        if contains(switch, p) {
            match detail {
                Some(Detail::Wifi) => {
                    self.apply_tile(Ctl::Wifi, p);
                    let mut st = self.ivars().state.borrow_mut();
                    st.nets.clear();
                    st.scanning = true;
                    st.next_scan = Some(Instant::now() + std::time::Duration::from_secs(4));
                }
                Some(Detail::Bluetooth) => {
                    self.apply_tile(Ctl::Bluetooth, p);
                    let st = self.ivars().state.borrow();
                    let _ = st.req_tx.send(nets::Req::ListBt);
                }
                None => {}
            }
            return self.setNeedsDisplay(true);
        }
        if !contains(self.list_area(), p) {
            return;
        }
        let Some((row, r)) = self.detail_rows(w).into_iter().find(|(_, r)| contains(*r, p)) else { return };
        match row {
            Row::LocationHint => {
                let st = self.ivars().state.borrow();
                let status = st.location.as_ref().map(|lm| unsafe { lm.authorizationStatus() });
                if status == Some(CLAuthorizationStatus::NotDetermined) {
                    if let Some(lm) = &st.location {
                        unsafe { lm.requestWhenInUseAuthorization() };
                    }
                } else {
                    drop(st);
                    open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices");
                    self.set_expanded(false);
                }
            }
            Row::Net(ssid) => {
                let mut st = self.ivars().state.borrow_mut();
                let Some(n) = st.nets.iter().find(|n| n.ssid == ssid).cloned() else { return };
                if n.current || st.joining.is_some() {
                    return;
                }
                if st.password_for.as_deref() == Some(&ssid) {
                    // click inside the expanded row: the join button
                    let join_btn = rect(r.origin.x + r.size.width - 78.0, r.origin.y + ROW_H, 72.0, 28.0);
                    drop(st);
                    if contains(join_btn, p) {
                        self.submit_password();
                    }
                    return;
                }
                st.join_failed = None;
                if n.known || !n.secure {
                    st.joining = Some(ssid.clone());
                    let _ = st.req_tx.send(nets::Req::Join { ssid, password: None });
                    drop(st);
                    self.hide_password_field();
                } else {
                    st.password_for = Some(ssid);
                    drop(st);
                    self.show_password_field();
                }
            }
            Row::Bt(addr) => {
                let mut st = self.ivars().state.borrow_mut();
                if st.bt_busy.is_some() {
                    return;
                }
                let Some(d) = st.bt_devs.iter().find(|d| d.addr == addr).cloned() else { return };
                st.bt_busy = Some(addr.clone());
                let _ = st.req_tx.send(nets::Req::SetBt { addr, connect: !d.connected });
            }
        }
        self.setNeedsDisplay(true);
    }

    fn scroll_list(&self, dy: f64) {
        let w = self.inner_width();
        let content = {
            let rows = self.detail_rows(w);
            let st = self.ivars().state.borrow();
            rows.last().map(|(_, r)| r.origin.y + r.size.height + st.list_scroll).unwrap_or(0.0)
        };
        let area = self.list_area();
        let max = (content - (area.origin.y + area.size.height) + 6.0).max(0.0);
        {
            let mut st = self.ivars().state.borrow_mut();
            if st.password_for.is_some() {
                return; // keep the text field where it is
            }
            st.list_scroll = (st.list_scroll - dy).clamp(0.0, max);
        }
        self.setNeedsDisplay(true);
    }

    fn show_password_field(&self) {
        let w = self.inner_width();
        let mtm = self.mtm();
        let ssid = self.ivars().state.borrow().password_for.clone();
        let Some(ssid) = ssid else { return };
        let Some((_, r)) = self.detail_rows(w).into_iter().find(|(row, _)| *row == Row::Net(ssid.clone())) else {
            return;
        };
        self.hide_password_field();
        let field = NSSecureTextField::new(mtm);
        // view coords include the ear offset
        field.setFrame(rect(r.origin.x + 30.0 + EAR, r.origin.y + ROW_H + 2.0, r.size.width - 30.0 - 86.0, 24.0));
        field.setBezeled(false);
        field.setDrawsBackground(true);
        field.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.12)));
        field.setTextColor(Some(&NSColor::whiteColor()));
        field.setFont(Some(&font(13.0, 0.0)));
        field.setFocusRingType(NSFocusRingType::None);
        field.setPlaceholderString(Some(&NSString::from_str("รหัสผ่าน")));
        unsafe {
            field.setTarget(Some(self));
            field.setAction(Some(sel!(submitPassword:)));
        }
        self.addSubview(&field);
        WANT_KEY.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(win) = self.window() {
            win.makeKeyWindow();
            win.makeFirstResponder(Some(&field));
        }
        self.ivars().state.borrow_mut().pw_field = Some(field);
    }

    fn hide_password_field(&self) {
        let field = self.ivars().state.borrow_mut().pw_field.take();
        if let Some(f) = field {
            f.removeFromSuperview();
        }
        let ask_up = self.ivars().state.borrow().ask_field.is_some();
        WANT_KEY.store(ask_up, std::sync::atomic::Ordering::SeqCst);
    }

    fn submit_password(&self) {
        let (field, ssid) = {
            let st = self.ivars().state.borrow();
            (st.pw_field.clone(), st.password_for.clone())
        };
        let (Some(field), Some(ssid)) = (field, ssid) else { return };
        let pw = field.stringValue().to_string();
        if pw.is_empty() {
            return;
        }
        self.hide_password_field();
        let mut st = self.ivars().state.borrow_mut();
        st.password_for = None;
        st.joining = Some(ssid.clone());
        let _ = st.req_tx.send(nets::Req::Join { ssid, password: Some(pw) });
        drop(st);
        self.setNeedsDisplay(true);
    }

    /// Perform a control's single-click action.
    fn apply_tile(&self, ctl: Ctl, p: NSPoint) {
        use controls::Action;
        let mut st = self.ivars().state.borrow_mut();
        let Some(c) = st.controls.as_mut() else { return };
        let action = match ctl {
            Ctl::Wifi => {
                c.wifi = !c.wifi;
                c.wifi_connected = false;
                Some(Action::Wifi(c.wifi))
            }
            Ctl::Bluetooth => c.bluetooth.as_mut().map(|b| {
                *b = !*b;
                Action::Bluetooth(*b)
            }),
            Ctl::Dark => {
                c.dark = !c.dark;
                Some(Action::Dark(c.dark))
            }
            Ctl::Mute => {
                c.muted = !c.muted;
                Some(Action::Mute(c.muted))
            }
            Ctl::Volume | Ctl::Brightness => {
                st.dragging = Some(ctl);
                drop(st);
                return self.drag_to(p);
            }
            Ctl::AirDrop => {
                drop(st);
                self.set_expanded(false);
                pick_files_for_airdrop(self.mtm());
                return;
            }
        };
        st.controls_touched = Some(Instant::now());
        st.pressed = Some((Btn::Tab(Page::Controls), Instant::now()));
        if let Some(a) = action {
            let _ = st.action_tx.send(a);
        }
        drop(st);
        self.setNeedsDisplay(true);
    }

    /// Slider drag (volume / brightness).
    fn drag_to(&self, p: NSPoint) {
        let w = self.inner_width();
        let mut st = self.ivars().state.borrow_mut();
        let Some(which) = st.dragging else { return };
        let l = ctl_layout(w, st.geo.notch_h);
        st.controls_touched = Some(Instant::now());
        let Some(c) = st.controls.as_mut() else { return };
        match which {
            Ctl::Volume => {
                let v = slider_value(l.vol_track, p.x);
                c.volume = v;
                c.muted = false;
                let _ = st.action_tx.send(controls::Action::Volume(v));
            }
            Ctl::Brightness => {
                let v = slider_value(l.bri_track, p.x);
                c.brightness = Some(v);
                controls::set_brightness(v);
            }
            _ => {}
        }
        drop(st);
        self.setNeedsDisplay(true);
    }

    /// Called ~30×/s by the timer on the main thread.
    fn tick(&self) {
        let mut changed_shape = false;
        let mut redraw = false;
        let mut show_pw = false;
        let mut answer_arrived = false;
        {
            let mut st = self.ivars().state.borrow_mut();
            while let Ok(msg) = st.msg_rx.try_recv() {
                match msg {
                    Msg::Track(t) => {
                        changed_shape |= st.track.is_some() != t.is_some();
                        if let Some(app) = t.as_ref().map(|t| t.app) {
                            if st.app_icon.as_ref().map(|(a, _)| *a) != Some(app) {
                                st.app_icon = app_icon(app).map(|i| (app, i));
                            }
                        }
                        let mut t = t;
                        if let (Some(tr), Some((p, at))) = (t.as_mut(), st.optimistic) {
                            if at.elapsed().as_secs_f64() < 1.5 {
                                tr.playing = p;
                            } else {
                                st.optimistic = None;
                            }
                        }
                        if let (Some(tr), Some((v, at))) = (t.as_mut(), st.vol_target) {
                            if at.elapsed().as_secs_f64() < 2.0 {
                                tr.volume = Some(v);
                            } else {
                                st.vol_target = None;
                            }
                        }
                        if let (Some(tr), Some((secs, at))) = (t.as_mut(), st.seek_target) {
                            let age = at.elapsed().as_secs_f64();
                            if age < 2.0 {
                                tr.position = secs + if tr.playing { age } else { 0.0 };
                            } else {
                                st.seek_target = None;
                            }
                        }
                        st.track = t;
                        st.track_at = Instant::now();
                    }
                    Msg::Artwork(bytes) => {
                        st.art = bytes.and_then(|b| {
                            NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&b))
                        });
                        st.accent = st.art.as_deref().and_then(accent_from).unwrap_or(DEFAULT_ACCENT);
                        st.art_blur = st.art.as_deref().and_then(blurred);
                    }
                }
                redraw = true;
            }
            while let Ok(r) = st.ask_rx.try_recv() {
                st.ask_pending = false;
                match r {
                    Ok(a) => {
                        st.ask_answer = Some(if a.text.is_empty() { "(ไม่มีคำตอบ)".into() } else { a.text });
                        if a.session.is_some() {
                            st.ask_session = a.session;
                        }
                    }
                    Err(e) => st.ask_answer = Some(format!("⚠️ {e}")),
                }
                answer_arrived = true;
            }
            while let Ok(l) = st.limits_rx.try_recv() {
                // keep the last good numbers if a refresh fails
                if l.is_ok() || !matches!(st.limits, Some(Ok(_))) {
                    st.limits = Some(l);
                }
                redraw = true;
            }
            while let Ok(x) = st.sys_rx.try_recv() {
                st.sys = Some(x);
                if st.expanded {
                    redraw = true;
                }
            }
            while let Ok(p) = st.privacy_rx.try_recv() {
                changed_shape |= privacy_width(&st.privacy) != privacy_width(&p);
                st.privacy = p;
                redraw = true;
            }
            while let Ok(sessions) = st.status_rx.try_recv() {
                changed_shape |= st.sessions.is_empty() != sessions.is_empty();
                st.sessions = sessions;
                redraw = true;
            }
            if st.sessions.iter().any(|s| s.busy()) {
                // Claude Code style spinner: a new glyph every 120 ms
                let frame = (st.started.elapsed().as_millis() / 120) as usize % SPINNER.len();
                if frame != st.spin_frame {
                    st.spin_frame = frame;
                    redraw = true;
                }
            }
            // ---- banners ----
            let mut new_banner: Option<Banner> = None;
            // Charger plugged in? (only the battery → AC transition)
            if power::take_changed() {
                let ac = power::on_ac();
                if ac && !st.on_ac {
                    let pct = controls::battery().map(|b| b.0).unwrap_or(0);
                    new_banner = Some(Banner::Charging(pct));
                    st.low_warned = 100;
                }
                st.on_ac = ac;
            }
            // Battery low (20% and again at 10%), only while on battery.
            if let Some((pct, charging)) = st.controls.as_ref().and_then(|c| c.battery) {
                if charging || st.on_ac {
                    st.low_warned = 100;
                } else if pct <= 10 && st.low_warned > 10 {
                    st.low_warned = 10;
                    new_banner = Some(Banner::LowBattery(pct));
                } else if pct <= 20 && st.low_warned > 20 {
                    st.low_warned = 20;
                    new_banner = Some(Banner::LowBattery(pct));
                }
            }
            // Volume / brightness keys → HUD (poll ~10×/s; both reads are cheap)
            st.hud_poll = st.hud_poll.wrapping_add(1);
            if st.hud_poll % 3 == 0 {
                let ours = st.dragging.is_some()
                    || st.controls_touched.is_some_and(|t| t.elapsed().as_secs_f64() < 1.0);
                if let Some(v) = audio::output_volume() {
                    if let Some(old) = st.last_volume {
                        if ((v.0 - old.0).abs() > 0.004 || v.1 != old.1) && !ours {
                            new_banner = Some(Banner::Volume(v.0 as f64, v.1));
                        }
                    }
                    st.last_volume = Some(v);
                }
                if let Some(b) = controls::brightness() {
                    // auto-brightness drifts slowly; key presses jump ~6%
                    if let Some(old) = st.last_brightness {
                        if (b - old).abs() > 0.02 && !ours {
                            new_banner = Some(Banner::Brightness(b));
                        }
                    }
                    st.last_brightness = Some(b);
                }
            }
            if let Some(nb) = new_banner.filter(|_| !st.expanded) {
                let now = Instant::now();
                st.banner = Some(match st.banner {
                    // same kind of HUD already up: just update the value
                    Some((old, since, _)) if old.is_level() && nb.is_level()
                        && std::mem::discriminant(&old) == std::mem::discriminant(&nb) => (nb, since, now),
                    Some((old, ..)) => {
                        changed_shape |= old.wing() != nb.wing();
                        (nb, now, now)
                    }
                    None => {
                        changed_shape = true;
                        (nb, now, now)
                    }
                });
                redraw = true;
            }
            if let Some((b, _, updated)) = st.banner {
                if updated.elapsed().as_secs_f64() > b.secs() || st.expanded {
                    st.banner = None;
                    changed_shape = true;
                } else {
                    redraw = true; // fill animations / fade
                }
            }
            if let Some((ctl, at)) = st.pending_tile {
                if Instant::now() >= at {
                    st.pending_tile = None;
                    drop(st);
                    self.apply_tile(ctl, NSPoint::ZERO);
                    st = self.ivars().state.borrow_mut();
                }
            }
            while let Ok(r) = st.resp_rx.try_recv() {
                match r {
                    nets::Resp::Wifi(res) => {
                        st.scanning = false;
                        match res {
                            Ok(v) => {
                                st.nets = v;
                                st.wifi_err = None;
                            }
                            Err(e) => st.wifi_err = Some(e),
                        }
                    }
                    nets::Resp::Joined { ssid, ok } => {
                        st.joining = None;
                        if !ok {
                            // saved password missing / wrong → ask for it
                            st.join_failed = Some(ssid.clone());
                            if st.detail == Some(Detail::Wifi) {
                                st.password_for = Some(ssid);
                                show_pw = true;
                            }
                        }
                    }
                    nets::Resp::Bt(v) => st.bt_devs = v,
                    nets::Resp::BtDone => st.bt_busy = None,
                }
                redraw = true;
            }
            if st.detail == Some(Detail::Wifi) && st.expanded && st.joining.is_none() {
                if st.next_scan.is_some_and(|t| Instant::now() >= t) {
                    st.scanning = true;
                    let _ = st.req_tx.send(nets::Req::ScanWifi);
                    st.next_scan = Some(Instant::now() + std::time::Duration::from_secs(12));
                }
            }
            if (st.scanning || st.joining.is_some() || st.bt_busy.is_some()) && st.detail.is_some() {
                let frame = (st.started.elapsed().as_millis() / 120) as usize % SPINNER.len();
                if frame != st.spin_frame {
                    st.spin_frame = frame;
                    redraw = true;
                }
            }
            while let Ok(c) = st.controls_rx.try_recv() {
                let fresh = st.controls_touched.is_none_or(|t| t.elapsed().as_secs_f64() > 2.5);
                if fresh && st.dragging.is_none() {
                    st.controls = Some(c);
                    redraw = true;
                }
            }
            while let Ok(u) = st.usage_rx.try_recv() {
                st.usage = Some(u);
                redraw = true;
            }
            if st.page_changed.elapsed().as_secs_f64() < 0.3 {
                redraw = true;
            }
            // Claude page shows a countdown: refresh it about once a second.
            if st.expanded && st.page == Page::Claude && st.started.elapsed().subsec_millis() < 34 {
                redraw = true;
            }
            // Keep animating the equaliser while something plays (or might).
            let playing = st.track.as_ref().is_some_and(|t| t.playing);
            if playing != st.shown_playing {
                st.glyph_changed = Some((st.shown_playing, Instant::now()));
                st.shown_playing = playing;
            }
            if st.glyph_changed.is_some_and(|(_, t)| t.elapsed().as_secs_f64() < 0.3) {
                redraw = true;
            }
            // Nothing animates while playing; just keep the time/progress
            // bar current (~2×/s) when it's visible.
            let progress_visible = st.expanded
                && st.page == Page::Music
                && st.track.as_ref().is_some_and(|t| t.full && t.duration > 0.0);
            if playing && progress_visible && st.started.elapsed().subsec_millis() % 500 < 34 {
                redraw = true;
            }
            if st.pressed.is_some_and(|(_, t)| t.elapsed().as_secs_f64() < 0.35) {
                redraw = true;
            }
        }
        {
            // Idle → fold back to the bare notch. "Active" = music playing or a
            // Claude session to show; keep the wings a few seconds after that
            // so a quick pause / skip doesn't make the island jump.
            let mut st = self.ivars().state.borrow_mut();
            let playing = st.track.as_ref().is_some_and(|t| t.playing);
            if playing || !st.sessions.is_empty() {
                st.last_active = Instant::now();
            }
            let wings = st.track.is_some() && st.last_active.elapsed().as_secs_f64() < IDLE_SECS
                || !st.sessions.is_empty()
                || !st.privacy.is_empty();
            if wings != st.wings {
                st.wings = wings;
                changed_shape = true;
            }
        }
        if show_pw {
            self.show_password_field();
        }
        if answer_arrived {
            self.refresh_answer();
        }
        self.tick_weather();
        self.tick_camera();
        self.tick_ask();
        if self.tick_eq() {
            // only repaint the little bars, not the whole island
            self.setNeedsDisplayInRect(self.eq_dirty_rect());
        }
        if self.update_hover() {
            changed_shape = true;
        }
        // The open island only closes on a click elsewhere (global monitor)
        // or on the notch strip — moving the mouse away keeps it open.
        if changed_shape {
            self.relayout();
        }
        if redraw {
            self.setNeedsDisplay(true);
        }
    }

    /// Track whether the cursor is over the collapsed island. Returns true if
    /// that changed (→ bulge out / back in).
    fn update_hover(&self) -> bool {
        let mut st = self.ivars().state.borrow_mut();
        let Some(win) = st.window.clone() else { return false };
        let hovering = if st.expanded || st.banner.is_some() {
            false
        } else {
            let m = NSEvent::mouseLocation();
            let f = win.frame();
            // a little slack once bulged, so it doesn't flicker at the edge;
            // the screen's top edge counts as inside
            let slack = if st.hovering { 4.0 } else { 0.0 };
            m.x >= f.origin.x + EAR - slack
                && m.x <= f.origin.x + f.size.width - EAR + slack
                && m.y >= f.origin.y - slack
        };
        if hovering != st.hovering {
            st.hovering = hovering;
            return true;
        }
        false
    }

    fn snapshot(&self) -> Frame {
        let st = self.ivars().state.borrow();
        let (pos, dur) = Self::position_now(&st);
        Frame {
            geo: st.geo,
            page: st.page,
            page_t: (st.page_changed.elapsed().as_secs_f64() / 0.22).min(1.0),
            usage: st.usage.clone(),
            sessions: st.sessions.clone(),
            spin_frame: st.spin_frame,
            controls: st.controls.clone(),
            seek_drag: st.seek_drag,
            bar_hover: st.bar_hover,
            vol_drag: st.vol_drag,
            vol_hover: st.vol_hover,
            art_blur: st.art_blur.clone(),
            weather: st.weather.clone(),
            weather_err: st.weather_err.clone(),
            place: st.place.clone(),
            loc_status: st.location.as_ref().map(|l| unsafe { l.authorizationStatus() }),
            cam_on: st.cam_on,
            cam_denied: st.cam_denied,
            detail: st.detail,
            nets: st.nets.clone(),
            scanning: st.scanning,
            joining: st.joining.clone(),
            join_failed: st.join_failed.clone(),
            password_for: st.password_for.clone(),
            bt_devs: st.bt_devs.clone(),
            bt_busy: st.bt_busy.clone(),
            banner: st.banner,
            privacy: st.privacy.clone(),
            eq: st.tap.as_ref().map(|_| st.eq_levels),
            sys: st.sys,
            limits: st.limits.clone(),
            shelf: st.shelf.clone(),
            shelf_hover: st.shelf_hover,
            drop_to_shelf: st.drop_to_shelf,
            ask_open: st.ask_answer.is_some() || st.ask_pending,
            battery: st.controls.as_ref().and_then(|c| c.battery).map(|(p, c)| (p, c || st.on_ac)),
            drop_hover: st.drop_hover,
            wings: st.wings,
            track: st.track.clone(),
            art: st.art.clone(),
            accent: st.accent,
            icon: st.app_icon.as_ref().map(|(_, i)| i.clone()),
            hover: st.hover,
            pressed: st.pressed,
            glyph_changed: st.glyph_changed,
            pos,
            dur,
        }
    }

    fn draw(&self) {
        let f = self.snapshot();
        let b = self.bounds();
        let (full_w, h) = (b.size.width, b.size.height);
        let w = full_w - 2.0 * EAR;

        // How far we are into the expand animation (0 = collapsed, 1 = open).
        let t = ((h - f.geo.notch_h) / EXP_BODY_H).clamp(0.0, 1.0);
        let radius = 10.0 + 14.0 * t;

        // ----- body -----
        let body = island_path(full_w, h, radius);
        NSColor::blackColor().setFill();
        body.fill();

        NSGraphicsContext::saveGraphicsState_class();
        body.addClip();
        let tr = NSAffineTransform::transform();
        tr.translateXBy_yBy(EAR, 0.0);
        tr.concat();

        let l = layout(w, f.geo.notch_h);
        let animate_eq = f.track.as_ref().is_some_and(|t| t.playing);

        // blurred-artwork backdrop on the music page (Apple Music style)
        if t > 0.05 && !f.drop_hover && f.page == Page::Music && f.track.is_some() {
            if let Some(bg) = &f.art_blur {
                if let Some(ctx) = NSGraphicsContext::currentContext() {
                    ctx.setImageInterpolation(NSImageInterpolation::High);
                }
                let body_r = rect(-EAR, 0.0, full_w, h);
                draw_image(bg, body_r, NSRect::ZERO, 0.85 * t);
                // darken + fade to black towards the bottom so text stays readable
                NSColor::colorWithWhite_alpha(0.0, 0.45 * t).setFill();
                NSBezierPath::fillRect(body_r);
                if let Some(g) = NSGradient::initWithStartingColor_endingColor(
                    NSGradient::alloc(),
                    &NSColor::colorWithWhite_alpha(0.0, 0.0),
                    &NSColor::colorWithWhite_alpha(0.0, 0.55 * t),
                ) {
                    g.drawInRect_angle(body_r, 90.0);
                }
            }
        }
        // ----- collapsed "wings" -----
        let claude = f.sessions.first();
        if let (Some((b, since, updated)), true) = (f.banner, t < 0.5) {
            let age = since.elapsed().as_secs_f64();
            let left = b.secs() - updated.elapsed().as_secs_f64();
            // fade in once the island has grown, fade out at the end
            let a = ((age - 0.15) / 0.25).clamp(0.0, 1.0) * ((left - 0.05) / 0.25).clamp(0.0, 1.0);
            match b {
                Banner::Charging(pct) => self.draw_battery_banner(w, f.geo, age, a, pct, true),
                Banner::LowBattery(pct) => self.draw_battery_banner(w, f.geo, age, a, pct, false),
                Banner::Volume(v, muted) => self.draw_level(w, f.geo, a, v, muted, true),
                Banner::Brightness(v) => self.draw_level(w, f.geo, a, v, false, false),
            }
        }
        if f.banner.is_none()
            && t < 0.5
            && f.wings
            && w > f.geo.notch_w + 10.0
            && (f.track.is_some() || claude.is_some() || !f.privacy.is_empty())
        {
            let a = 1.0 - t * 2.0;
            let nh = f.geo.notch_h;
            let s = (nh - 10.0).min(24.0);
            let pw = privacy_width(&f.privacy);
            // left wing = [privacy group][music art / Claude], anchored left
            self.draw_privacy(&f.privacy, rect(6.0, 0.0, pw, nh), a);
            let content = WING;
            let left = rect(pw + (content - s) / 2.0 + 2.0, (nh - s) / 2.0, s, s);
            // right wing = [eq / Claude][battery], anchored right
            let side = WING;
            let right = rect(w - WING - 2.0, 0.0, WING, nh);
            if f.track.is_some() {
                // left: artwork, right: Claude status if working, else equaliser
                match &f.art {
                    Some(art) => draw_artwork(art, left, 6.0, a),
                    None => {
                        if let Some(i) = self.symbol("music.note", 13.0, (1.0, 1.0, 1.0, 0.9)) {
                            draw_centered(&i, left, a, 1.0);
                        }
                    }
                }
                match claude {
                    Some(c) => self.draw_claude_indicator(c, f.spin_frame, right, 16.0, a),
                    None => {
                        let eq = rect(right.origin.x + (side - 18.0) / 2.0, (nh - 12.0) / 2.0, 18.0, 12.0);
                        draw_eq(eq, f.accent, a, animate_eq, f.eq);
                    }
                }
            } else if let Some(c) = claude {
                // no music: left = spinner, right = elapsed time
                self.draw_claude_indicator(c, f.spin_frame, rect(pw + 2.0, 0.0, content, nh), 16.0, a);
                let label = if c.activity == status::Activity::Done {
                    String::new()
                } else {
                    fmt_elapsed(status::unix_now() - c.since)
                };
                draw_text(
                    &label,
                    rect(right.origin.x, (nh - 14.0) / 2.0, side, 14.0),
                    &NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe { NSFontWeightMedium }),
                    &rgb(CLAUDE_ORANGE, a),
                    NSTextAlignment::Center,
                );
            }
        }

        // ----- expanded content -----
        if t > 0.35 {
            let a = ((t - 0.35) / 0.65).powi(2);
            self.draw_tabs(&f, a);
            let batt_x = w - PAD - BATT_W + 8.0;
            if let Some((pct, charging)) = f.battery {
                self.draw_battery(rect(batt_x, 0.0, BATT_W, f.geo.notch_h), pct, charging, a);
            }
            if let Some(x) = f.sys {
                self.draw_sysbar(&x, batt_x - 4.0, f.geo.notch_h, a);
            }
            let a = a * (1.0 - (1.0 - f.page_t).powi(2));
            let a_page = if f.drop_hover { 0.0 } else { a };
            let a = a_page;
            match f.page {
                Page::Music => self.draw_expanded(&f, &l, w, a),
                Page::Claude => self.draw_claude(&f, w, a),
                Page::Shelf => self.draw_shelf(&f, w, a),
                Page::Weather => self.draw_weather(&f, w, a),
                Page::Camera => self.draw_camera(&f, w, a),
                Page::Controls if f.detail.is_some() => self.draw_detail(&f, w, a),
                Page::Controls => self.draw_controls(&f, w, a),
            }
            if f.drop_hover {
                self.draw_drop_zone(&f, w, ((t - 0.35) / 0.65).clamp(0.0, 1.0));
            }
        }

        NSGraphicsContext::restoreGraphicsState_class();
    }

    /// iPhone-style battery banner: "⚡ กำลังชาร์จ 87%" (green) or
    /// "แบตเหลือน้อย 15%" (red).
    fn draw_battery_banner(&self, w: f64, g: Geometry, age: f64, a: f64, pct: u8, charging: bool) {
        if a <= 0.0 {
            return;
        }
        let nh = g.notch_h;
        let col = if charging { CHARGE_GREEN } else { LOW_RED };
        let side = (w - g.notch_w) / 2.0;

        // left: icon + label
        let icon = if charging { "bolt.fill" } else { "exclamationmark.triangle.fill" };
        if let Some(i) = self.symbol(icon, 13.0, (col.0, col.1, col.2, 1.0)) {
            draw_centered(&i, rect(14.0, 0.0, 18.0, nh), a, 1.0);
        }
        draw_text(
            if charging { "กำลังชาร์จ" } else { "แบตเหลือน้อย" },
            rect(36.0, (nh - 17.0) / 2.0, side - 40.0, 17.0),
            &font(13.0, unsafe { NSFontWeightSemibold }),
            &white(a),
            NSTextAlignment::Left,
        );

        // right: 87% [■■■■□] — the charging one fills up from empty
        let fill = if charging {
            ease_out_cubic(((age - 0.3) / 0.9).clamp(0.0, 1.0)) * pct as f64 / 100.0
        } else {
            pct as f64 / 100.0
        };
        let bw = 26.0;
        let bh = 12.5;
        let bx = w - 14.0 - bw - 3.0;
        let by = (nh - bh) / 2.0;
        let body = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(bx, by, bw, bh), 3.5, 3.5);
        white(0.45 * a).setStroke();
        body.setLineWidth(1.2);
        body.stroke();
        fill_round(rect(bx + bw + 1.2, by + bh / 2.0 - 2.5, 2.2, 5.0), 1.0, &white(0.45 * a));
        let inner = rect(bx + 2.2, by + 2.2, ((bw - 4.4) * fill).max(2.0), bh - 4.4);
        fill_round(inner, 1.8, &rgb(col, a));
        let shown = (fill * 100.0).round() as u8;
        draw_text(
            &format!("{shown}%"),
            rect(bx - 54.0, (nh - 17.0) / 2.0, 48.0, 17.0),
            &NSFont::monospacedDigitSystemFontOfSize_weight(13.0, unsafe { NSFontWeightSemibold }),
            &rgb(col, a),
            NSTextAlignment::Right,
        );
    }

    /// Volume / brightness HUD: icon on the left, level bar on the right.
    fn draw_level(&self, w: f64, g: Geometry, a: f64, v: f64, muted: bool, volume: bool) {
        if a <= 0.0 {
            return;
        }
        let nh = g.notch_h;
        let side = (w - g.notch_w) / 2.0;
        let v = if muted { 0.0 } else { v.clamp(0.0, 1.0) };
        let icon = if volume {
            if muted || v == 0.0 {
                "speaker.slash.fill"
            } else if v < 0.34 {
                "speaker.wave.1.fill"
            } else if v < 0.67 {
                "speaker.wave.2.fill"
            } else {
                "speaker.wave.3.fill"
            }
        } else if v < 0.5 {
            "sun.min.fill"
        } else {
            "sun.max.fill"
        };
        if let Some(i) = self.symbol(icon, 14.0, (1.0, 1.0, 1.0, 0.95)) {
            draw_centered(&i, rect(0.0, 0.0, side, nh), a, 1.0);
        }
        // level bar centred in the right wing
        let bw = side - 30.0;
        let bar = rect(w - side + (side - bw) / 2.0, (nh - 6.0) / 2.0, bw, 6.0);
        fill_round(bar, 3.0, &white(0.2 * a));
        if v > 0.0 {
            let mut done = bar;
            done.size.width = (bar.size.width * v).max(6.0);
            fill_round(done, 3.0, &white(a));
        }
    }

    /// Drop target while files are dragged over the island:
    /// left half = park on the shelf, right half = AirDrop.
    fn draw_drop_zone(&self, f: &Frame, w: f64, a: f64) {
        let top = f.geo.notch_h;
        let gap = 10.0;
        let zw = (w - 2.0 * PAD - gap) / 2.0;
        let zones = [
            (rect(PAD, top + 10.0, zw, EXP_BODY_H - 24.0), true, "tray.and.arrow.down.fill", "พักไว้ในชั้นวาง", (0.95, 0.75, 0.35)),
            (rect(PAD + zw + gap, top + 10.0, zw, EXP_BODY_H - 24.0), false, "dot.radiowaves.up.forward", "ส่งผ่าน AirDrop", (0.45, 0.7, 1.0)),
        ];
        for (zone, is_shelf, icon, label, col) in zones {
            let active = f.drop_to_shelf == is_shelf;
            fill_round(zone, 16.0, &rgb(col, if active { 0.2 } else { 0.06 } * a));
            let border = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(zone, 16.0, 16.0);
            border.setLineWidth(1.5);
            let dash: [f64; 2] = [7.0, 5.0];
            unsafe { border.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
            rgb(col, if active { 0.9 } else { 0.35 } * a).setStroke();
            border.stroke();
            if let Some(i) = self.symbol(icon, 26.0, (col.0, col.1, col.2, if active { 1.0 } else { 0.5 })) {
                draw_centered(&i, rect(zone.origin.x, zone.origin.y + 16.0, zone.size.width, 42.0), a, 1.0);
            }
            draw_text(label, rect(zone.origin.x, zone.origin.y + 66.0, zone.size.width, 20.0), &font(14.0, unsafe { NSFontWeightSemibold }), &white(if active { 1.0 } else { 0.55 } * a), NSTextAlignment::Center);
        }
    }

    /// Parked files: icon + name, × to remove, drag out to use, click to open.
    fn draw_shelf(&self, f: &Frame, w: f64, a: f64) {
        let top = f.geo.notch_h;
        let n = f.shelf.len();
        draw_text(
            &if n == 0 { "ชั้นวาง".to_string() } else { format!("ชั้นวาง · {n} ไฟล์") },
            rect(PAD, top + 10.0, 200.0, 16.0),
            &font(12.0, unsafe { NSFontWeightMedium }),
            &white(0.5 * a),
            NSTextAlignment::Left,
        );
        if n == 0 {
            if let Some(i) = self.symbol("tray", 24.0, (1.0, 1.0, 1.0, 0.3)) {
                draw_centered(&i, rect(0.0, top + 38.0, w, 34.0), a, 1.0);
            }
            draw_text("ลากไฟล์มาวางที่ notch ฝั่งซ้าย เพื่อพักไว้ที่นี่", rect(0.0, top + 78.0, w, 18.0), &font(13.0, 0.0), &white(0.55 * a), NSTextAlignment::Center);
            draw_text("แล้วค่อยลากออกไปวางที่ไหนก็ได้", rect(0.0, top + 97.0, w, 16.0), &font(11.5, 0.0), &white(0.35 * a), NSTextAlignment::Center);
            return;
        }
        draw_text("ล้างทั้งหมด", shelf_clear_rect(w, top), &font(11.5, unsafe { NSFontWeightMedium }), &white(0.5 * a), NSTextAlignment::Right);
        let rects = shelf_rects(w, top, n);
        for (i, r) in rects.iter().enumerate() {
            let path = &f.shelf[i];
            let hovered = f.shelf_hover == Some(i);
            if hovered {
                fill_round(*r, 12.0, &white(0.08 * a));
            }
            let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&path.to_string_lossy()));
            draw_image(&icon, rect(r.origin.x + (r.size.width - 44.0) / 2.0, r.origin.y + 6.0, 44.0, 44.0), NSRect::ZERO, a);
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            draw_text(&name, rect(r.origin.x + 2.0, r.origin.y + 54.0, r.size.width - 4.0, 15.0), &font(10.5, 0.0), &white(0.85 * a), NSTextAlignment::Center);
            if hovered {
                let c = NSPoint::new(r.origin.x + r.size.width - 11.0, r.origin.y + 11.0);
                fill_circle(c, 8.0, &NSColor::colorWithWhite_alpha(0.25, a));
                if let Some(x) = self.symbol("xmark", 8.0, (1.0, 1.0, 1.0, 0.9)) {
                    draw_centered(&x, rect(c.x - 8.0, c.y - 8.0, 16.0, 16.0), a, 1.0);
                }
            }
        }
        let shown = rects.len();
        if shown < n {
            draw_text(&format!("+{} ไฟล์", n - shown), rect(w - PAD - 80.0, top + 118.0, 80.0, 14.0), &font(11.0, 0.0), &white(0.45 * a), NSTextAlignment::Right);
        }
    }

    /// iOS-style on/off switch.
    fn draw_switch(r: NSRect, on: bool, a: f64) {
        fill_round(r, r.size.height / 2.0, &*if on { rgb(CC_BLUE, a) } else { white(0.22 * a) });
        let k = r.size.height - 4.0;
        let x = if on { r.origin.x + r.size.width - k - 2.0 } else { r.origin.x + 2.0 };
        fill_circle(NSPoint::new(x + k / 2.0, r.origin.y + r.size.height / 2.0), k / 2.0, &white(a));
    }

    /// Wi-Fi network list / Bluetooth device list.
    fn draw_detail(&self, f: &Frame, w: f64, a: f64) {
        let Some(d) = f.detail else { return };
        let (back, switch) = self.detail_header(w);
        let on = match d {
            Detail::Wifi => f.controls.as_ref().is_some_and(|c| c.wifi),
            Detail::Bluetooth => f.controls.as_ref().is_some_and(|c| c.bluetooth == Some(true)),
        };
        // header: ‹ Wi-Fi ............ [spinner] (switch)
        if let Some(i) = self.symbol("chevron.left", 14.0, (1.0, 1.0, 1.0, 0.8)) {
            draw_centered(&i, rect(back.origin.x, back.origin.y, 24.0, back.size.height), a, 1.0);
        }
        draw_text(
            match d { Detail::Wifi => "Wi-Fi", Detail::Bluetooth => "Bluetooth" },
            rect(back.origin.x + 24.0, back.origin.y + 5.0, 120.0, 20.0),
            &font(15.0, unsafe { NSFontWeightBold }),
            &white(a),
            NSTextAlignment::Left,
        );
        Self::draw_switch(switch, on, a);
        if f.scanning && d == Detail::Wifi {
            draw_text(
                &format!("{} กำลังค้นหา", SPINNER[f.spin_frame % SPINNER.len()]),
                rect(switch.origin.x - 120.0, switch.origin.y + 3.0, 110.0, 16.0),
                &font(11.0, 0.0),
                &white(0.5 * a),
                NSTextAlignment::Right,
            );
        }
        white(0.08 * a).setFill();
        NSBezierPath::fillRect(rect(PAD - 6.0, back.origin.y + back.size.height + 3.0, w - 2.0 * PAD + 12.0, 1.0));

        // list (clipped)
        let area = self.list_area();
        NSGraphicsContext::saveGraphicsState_class();
        NSBezierPath::clipRect(area);
        let rows = self.detail_rows(w);
        let row_font = font(13.0, unsafe { NSFontWeightMedium });
        let small = font(11.0, 0.0);
        let blue = (CC_BLUE.0 + 0.25, CC_BLUE.1 + 0.2, 1.0);
        if !on {
            draw_text(
                match d { Detail::Wifi => "Wi-Fi ปิดอยู่", Detail::Bluetooth => "Bluetooth ปิดอยู่" },
                rect(0.0, area.origin.y + 40.0, w, 18.0),
                &font(13.0, 0.0),
                &white(0.45 * a),
                NSTextAlignment::Center,
            );
        } else if rows.is_empty() {
            let msg = match d {
                Detail::Wifi if f.scanning => "กำลังค้นหาเครือข่าย…",
                Detail::Wifi => "ไม่พบเครือข่าย",
                Detail::Bluetooth => "ยังไม่มีอุปกรณ์ที่จับคู่ไว้",
            };
            draw_text(msg, rect(0.0, area.origin.y + 40.0, w, 18.0), &font(13.0, 0.0), &white(0.45 * a), NSTextAlignment::Center);
        }
        for (row, r) in rows.iter().filter(|_| on) {
            if r.origin.y > area.origin.y + area.size.height || r.origin.y + r.size.height < area.origin.y {
                continue;
            }
            let ty = r.origin.y + (ROW_H - 2.0 - 17.0) / 2.0;
            match row {
                Row::LocationHint => {
                    fill_round(*r, 10.0, &white(0.07 * a));
                    if let Some(i) = self.symbol("location.fill", 14.0, (blue.0, blue.1, blue.2, 1.0)) {
                        draw_centered(&i, rect(r.origin.x + 8.0, r.origin.y, 24.0, r.size.height), a, 1.0);
                    }
                    draw_text("อนุญาต Location เพื่อดูชื่อ Wi-Fi", rect(r.origin.x + 38.0, r.origin.y + 8.0, r.size.width - 44.0, 17.0), &row_font, &white(a), NSTextAlignment::Left);
                    draw_text("macOS ซ่อนชื่อเครือข่ายจนกว่าจะอนุญาต · คลิกที่นี่", rect(r.origin.x + 38.0, r.origin.y + 27.0, r.size.width - 44.0, 15.0), &small, &white(0.5 * a), NSTextAlignment::Left);
                }
                Row::Net(ssid) => {
                    let Some(n) = f.nets.iter().find(|n| &n.ssid == ssid) else { continue };
                    if n.current {
                        fill_round(rect(r.origin.x, r.origin.y, r.size.width, ROW_H - 2.0), 9.0, &white(0.08 * a));
                    }
                    let strength = ((n.rssi as f64 + 90.0) / 40.0).clamp(0.15, 1.0);
                    if let Some(i) = self.symbol_var("wifi", strength, 13.0, (1.0, 1.0, 1.0, 0.9)) {
                        draw_centered(&i, rect(r.origin.x + 6.0, r.origin.y, 24.0, ROW_H - 2.0), a, 1.0);
                    }
                    let name_w = r.size.width - 30.0 - 130.0;
                    draw_text(ssid, rect(r.origin.x + 36.0, ty, name_w, 17.0), &*if n.current { font(13.0, unsafe { NSFontWeightBold }) } else { row_font.clone() }, &white(a), NSTextAlignment::Left);
                    let right = rect(r.origin.x + r.size.width - 130.0, ty, 122.0, 17.0);
                    if f.joining.as_deref() == Some(ssid.as_str()) {
                        draw_text(&format!("{} กำลังเชื่อมต่อ", SPINNER[f.spin_frame % SPINNER.len()]), right, &small, &white(0.6 * a), NSTextAlignment::Right);
                    } else if n.current {
                        if let Some(i) = self.symbol("checkmark", 12.0, (blue.0, blue.1, blue.2, 1.0)) {
                            draw_centered(&i, rect(right.origin.x + right.size.width - 18.0, r.origin.y, 18.0, ROW_H - 2.0), a, 1.0);
                        }
                    } else if f.join_failed.as_deref() == Some(ssid.as_str()) && f.password_for.as_deref() != Some(ssid.as_str()) {
                        draw_text("เชื่อมต่อไม่สำเร็จ", right, &small, &rgb((1.0, 0.45, 0.4), a), NSTextAlignment::Right);
                    } else if n.secure {
                        if let Some(i) = self.symbol("lock.fill", 10.0, (1.0, 1.0, 1.0, 0.45)) {
                            draw_centered(&i, rect(right.origin.x + right.size.width - 18.0, r.origin.y, 18.0, ROW_H - 2.0), a, 1.0);
                        }
                    }
                    if f.password_for.as_deref() == Some(ssid.as_str()) {
                        // the NSSecureTextField sits at the left; draw the join button
                        let btn = rect(r.origin.x + r.size.width - 78.0, r.origin.y + ROW_H, 72.0, 28.0);
                        fill_round(btn, 8.0, &rgb(CC_BLUE, a));
                        draw_text("เข้าร่วม", rect(btn.origin.x, btn.origin.y + 5.0, btn.size.width, 17.0), &font(12.0, unsafe { NSFontWeightSemibold }), &white(a), NSTextAlignment::Center);
                        if f.join_failed.as_deref() == Some(ssid.as_str()) {
                            draw_text("รหัสผ่านไม่ถูกต้องหรือหมดเวลา", rect(r.origin.x + 36.0, r.origin.y + ROW_H - 14.0, 220.0, 13.0), &font(10.0, 0.0), &rgb((1.0, 0.45, 0.4), a), NSTextAlignment::Left);
                        }
                    }
                }
                Row::Bt(addr) => {
                    let Some(dv) = f.bt_devs.iter().find(|d| &d.addr == addr) else { continue };
                    if dv.connected {
                        fill_round(*r, 9.0, &white(0.08 * a));
                    }
                    let icon = bt_icon(&dv.name);
                    let tint = if dv.connected { (blue.0, blue.1, blue.2, 1.0) } else { (1.0, 1.0, 1.0, 0.8) };
                    if let Some(i) = self.symbol(icon, 13.0, tint) {
                        draw_centered(&i, rect(r.origin.x + 6.0, r.origin.y, 24.0, r.size.height), a, 1.0);
                    }
                    draw_text(&dv.name, rect(r.origin.x + 36.0, ty, r.size.width - 30.0 - 130.0, 17.0), &row_font, &white(a), NSTextAlignment::Left);
                    let right = rect(r.origin.x + r.size.width - 130.0, ty, 122.0, 17.0);
                    let (label, col) = if f.bt_busy.as_deref() == Some(addr.as_str()) {
                        (format!("{} กำลังดำเนินการ", SPINNER[f.spin_frame % SPINNER.len()]), white(0.6 * a))
                    } else if dv.connected {
                        ("เชื่อมต่อแล้ว".to_string(), rgb(blue, a))
                    } else {
                        ("ไม่ได้เชื่อมต่อ".to_string(), white(0.45 * a))
                    };
                    draw_text(&label, right, &small, &col, NSTextAlignment::Right);
                }
            }
        }
        NSGraphicsContext::restoreGraphicsState_class();
    }

    /// SF Symbol with a variable value (e.g. Wi-Fi bars), tinted and cached.
    fn symbol_var(&self, name: &str, value: f64, size: f64, color: (f64, f64, f64, f64)) -> Option<Retained<NSImage>> {
        let v = (value * 4.0).round() / 4.0; // cache-friendly steps
        let key = format!("{name}|{v}|{size}|{color:?}");
        if let Some(i) = self.ivars().state.borrow().symbols.get(&key) {
            return Some(i.clone());
        }
        let img = NSImage::imageWithSystemSymbolName_variableValue_accessibilityDescription(&NSString::from_str(name), v, None)?;
        let cfg = unsafe { NSImageSymbolConfiguration::configurationWithPointSize_weight(size, NSFontWeightBold) };
        let col = NSColor::colorWithSRGBRed_green_blue_alpha(color.0, color.1, color.2, color.3);
        let cfg = cfg.configurationByApplyingConfiguration(&NSImageSymbolConfiguration::configurationWithHierarchicalColor(&col));
        let img = img.imageWithSymbolConfiguration(&cfg)?;
        self.ivars().state.borrow_mut().symbols.insert(key, img.clone());
        Some(img)
    }

    /// Multicolour SF Symbol (weather icons), cached.
    fn symbol_multi(&self, name: &str, size: f64) -> Option<Retained<NSImage>> {
        let key = format!("multi|{name}|{size}");
        if let Some(i) = self.ivars().state.borrow().symbols.get(&key) {
            return Some(i.clone());
        }
        let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
        let cfg = unsafe { NSImageSymbolConfiguration::configurationWithPointSize_weight(size, NSFontWeightMedium) }
            .configurationByApplyingConfiguration(&NSImageSymbolConfiguration::configurationPreferringMulticolor());
        let img = img.imageWithSymbolConfiguration(&cfg)?;
        self.ivars().state.borrow_mut().symbols.insert(key, img.clone());
        Some(img)
    }

    fn draw_weather(&self, f: &Frame, w: f64, a: f64) {
        let top = f.geo.notch_h;
        let gray = white(0.5 * a);
        let center_msg = |msg: &str, sub: &str| {
            draw_text(msg, rect(0.0, top + 50.0, w, 20.0), &font(14.0, unsafe { NSFontWeightSemibold }), &white(0.8 * a), NSTextAlignment::Center);
            draw_text(sub, rect(0.0, top + 72.0, w, 16.0), &font(11.5, 0.0), &white(0.45 * a), NSTextAlignment::Center);
        };
        let denied = matches!(f.loc_status, Some(s) if s == CLAuthorizationStatus::Denied || s == CLAuthorizationStatus::Restricted);
        let Some(wx) = &f.weather else {
            if denied {
                center_msg("อนุญาต Location เพื่อดูสภาพอากาศ", "คลิกเพื่อเปิดการตั้งค่า");
            } else if let Some(e) = &f.weather_err {
                center_msg(e, "จะลองใหม่อีกครั้ง");
            } else {
                center_msg("กำลังโหลดสภาพอากาศ…", "ใช้ตำแหน่งปัจจุบัน · ข้อมูลจาก Open-Meteo");
            }
            return;
        };
        let (icon, desc) = weather::describe(wx.code, wx.is_day);
        // left: big icon + temperature
        if let Some(i) = self.symbol_multi(icon, 40.0) {
            draw_centered(&i, rect(PAD - 4.0, top + 12.0, 64.0, 60.0), a, 1.0);
        }
        draw_text(
            &format!("{:.0}°", wx.temp),
            rect(PAD + 66.0, top + 10.0, 120.0, 50.0),
            &font(42.0, unsafe { NSFontWeightSemibold }),
            &white(a),
            NSTextAlignment::Left,
        );
        draw_text(desc, rect(PAD + 68.0, top + 60.0, 170.0, 17.0), &font(13.0, unsafe { NSFontWeightSemibold }), &white(0.85 * a), NSTextAlignment::Left);
        let place = f.place.clone().unwrap_or_else(|| "ตำแหน่งปัจจุบัน".into());
        if let Some(i) = self.symbol("location.fill", 9.0, (1.0, 1.0, 1.0, 0.5)) {
            draw_centered(&i, rect(PAD, top + 89.0, 12.0, 14.0), a, 1.0);
        }
        draw_text(&format!("{place} · รู้สึกเหมือน {:.0}°", wx.feels), rect(PAD + 16.0, top + 88.0, 230.0, 15.0), &font(11.5, 0.0), &gray, NSTextAlignment::Left);
        draw_text(
            &format!("สูง {:.0}° · ต่ำ {:.0}° · ความชื้น {:.0}%", wx.max, wx.min, wx.humidity),
            rect(PAD, top + 107.0, 250.0, 15.0),
            &font(11.5, 0.0),
            &gray,
            NSTextAlignment::Left,
        );

        // divider
        white(0.08 * a).setFill();
        let x1 = PAD + 262.0;
        NSBezierPath::fillRect(rect(x1 - 14.0, top + 14.0, 1.0, EXP_BODY_H - 28.0));

        // right: next hours
        let cw = (w - x1 - PAD) / wx.hours.len().max(1) as f64;
        let small = NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe { NSFontWeightMedium });
        for (i, h) in wx.hours.iter().enumerate() {
            let x = x1 + i as f64 * cw;
            draw_text(&format!("{:02}:00", h.hour), rect(x, top + 20.0, cw, 14.0), &small, &gray, NSTextAlignment::Center);
            let (hi, _) = weather::describe(h.code, h.is_day);
            if let Some(img) = self.symbol_multi(hi, 17.0) {
                draw_centered(&img, rect(x, top + 42.0, cw, 34.0), a, 1.0);
            }
            draw_text(&format!("{:.0}°", h.temp), rect(x, top + 86.0, cw, 18.0), &font(14.0, unsafe { NSFontWeightSemibold }), &white(a), NSTextAlignment::Center);
        }
    }

    /// Camera "mirror": the live preview is a subview; this draws what's
    /// behind it (frame, and messages when the camera isn't running).
    fn draw_camera(&self, f: &Frame, _w: f64, a: f64) {
        let r = self.camera_rect();
        fill_round(r, 16.0, &white(0.06 * a));
        let msg = if f.cam_denied {
            Some(("อนุญาตให้ Notch ใช้กล้อง", "คลิกเพื่อเปิดการตั้งค่า"))
        } else if !f.cam_on {
            Some(("กำลังเปิดกล้อง…", ""))
        } else {
            None
        };
        if let Some((m, sub)) = msg {
            if let Some(i) = self.symbol("camera.fill", 22.0, (1.0, 1.0, 1.0, 0.4)) {
                draw_centered(&i, rect(r.origin.x, r.origin.y + r.size.height / 2.0 - 46.0, r.size.width, 30.0), a, 1.0);
            }
            draw_text(m, rect(r.origin.x, r.origin.y + r.size.height / 2.0 - 8.0, r.size.width, 20.0), &font(14.0, unsafe { NSFontWeightSemibold }), &white(0.8 * a), NSTextAlignment::Center);
            draw_text(sub, rect(r.origin.x, r.origin.y + r.size.height / 2.0 + 14.0, r.size.width, 16.0), &font(11.5, 0.0), &white(0.45 * a), NSTextAlignment::Center);
        }
    }

    /// "87% [▮▮▮]" — green + bolt while charging, red at 20% or below.
    fn draw_battery(&self, r: NSRect, pct: u8, charging: bool, a: f64) {
        let nh = r.size.height;
        let col = if charging {
            CHARGE_GREEN
        } else if pct <= 20 {
            LOW_RED
        } else {
            (1.0, 1.0, 1.0)
        };
        let bw = 22.0;
        let bh = 11.0;
        let bx = r.origin.x + r.size.width - bw - 6.0;
        let by = (nh - bh) / 2.0;
        let body = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(bx, by, bw, bh), 3.0, 3.0);
        white(0.4 * a).setStroke();
        body.setLineWidth(1.0);
        body.stroke();
        fill_round(rect(bx + bw + 1.0, by + bh / 2.0 - 2.0, 1.8, 4.0), 0.9, &white(0.4 * a));
        let inner = rect(bx + 2.0, by + 2.0, ((bw - 4.0) * pct as f64 / 100.0).max(1.5), bh - 4.0);
        fill_round(inner, 1.5, &rgb(col, if charging || pct <= 20 { a } else { 0.9 * a }));
        if charging {
            if let Some(i) = self.symbol("bolt.fill", 8.0, (1.0, 1.0, 1.0, 1.0)) {
                draw_centered(&i, rect(bx, by, bw, bh), a, 1.0);
            }
        }
        draw_text(
            &format!("{pct}%"),
            rect(r.origin.x, (nh - 15.0) / 2.0, bx - r.origin.x - 4.0, 15.0),
            &NSFont::monospacedDigitSystemFontOfSize_weight(11.5, unsafe { NSFontWeightSemibold }),
            &*if charging || pct <= 20 { rgb(col, a) } else { white(0.9 * a) },
            NSTextAlignment::Right,
        );
    }

    /// 🎙[app icons]  📹 — like the iPhone's orange / green dots.
    fn draw_privacy(&self, p: &privacy::Privacy, r: NSRect, a: f64) {
        if p.is_empty() {
            return;
        }
        let nh = r.size.height;
        let x = r.origin.x + 4.0;
        if p.camera {
            let g = CAM_GREEN;
            if let Some(i) = self.symbol("video.fill", 10.0, (g.0, g.1, g.2, 1.0)) {
                draw_centered(&i, rect(x + 2.0, 0.0, 16.0, nh), a, 1.0);
            }
        }
    }


    /// Mini Control Center.
    fn draw_controls(&self, f: &Frame, w: f64, a: f64) {
        let top = f.geo.notch_h;
        let Some(c) = &f.controls else {
            draw_text("กำลังอ่านค่า…", rect(0.0, top + 60.0, w, 18.0), &font(13.0, 0.0), &white(0.45 * a), NSTextAlignment::Center);
            return;
        };
        let l = ctl_layout(w, top);
        let label_font = font(11.0, unsafe { NSFontWeightMedium });

        for (ctl, r) in l.tiles {
            let (on, icon, label): (bool, String, String) = match ctl {
                Ctl::Wifi => (
                    c.wifi,
                    if c.wifi { "wifi" } else { "wifi.slash" }.into(),
                    if !c.wifi { "Wi-Fi ปิด" } else if c.wifi_connected { "Wi-Fi" } else { "ไม่ได้เชื่อมต่อ" }.into(),
                ),
                Ctl::Bluetooth => (
                    c.bluetooth == Some(true),
                    "dot.radiowaves.left.and.right".into(),
                    match c.bluetooth {
                        Some(true) => "Bluetooth",
                        Some(false) => "Bluetooth ปิด",
                        None => "Bluetooth",
                    }
                    .into(),
                ),
                Ctl::Dark => (c.dark, "moon.fill".into(), if c.dark { "โหมดมืด" } else { "โหมดสว่าง" }.into()),
                _ => (false, "dot.radiowaves.up.forward".into(), "AirDrop".into()),
            };
            let bg = if on { rgb(CC_BLUE, a) } else { white(0.10 * a) };
            fill_round(r, 14.0, &bg);
            let tint = if ctl == Ctl::AirDrop {
                (CC_BLUE.0 + 0.2, CC_BLUE.1 + 0.15, 1.0, 1.0) // AirDrop blue
            } else {
                (1.0, 1.0, 1.0, if on { 1.0 } else { 0.8 })
            };
            if let Some(i) = self.symbol(&icon, 17.0, tint) {
                draw_centered(&i, rect(r.origin.x, r.origin.y + 6.0, r.size.width, 26.0), a, 1.0);
            }
            draw_text(
                &label,
                rect(r.origin.x + 4.0, r.origin.y + 36.0, r.size.width - 8.0, 15.0),
                &label_font,
                &white(if on { 1.0 } else { 0.7 } * a),
                NSTextAlignment::Center,
            );
        }

        // sliders
        let vol_icon = if c.muted || c.volume == 0.0 {
            "speaker.slash.fill"
        } else if c.volume < 0.34 {
            "speaker.wave.1.fill"
        } else if c.volume < 0.67 {
            "speaker.wave.2.fill"
        } else {
            "speaker.wave.3.fill"
        };
        let rows = [
            (l.vol_icon, l.vol_track, vol_icon, if c.muted { 0.0 } else { c.volume }, true),
            (l.bri_icon, l.bri_track, "sun.max.fill", c.brightness.unwrap_or(0.0), c.brightness.is_some()),
        ];
        let pct_font = NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe { NSFontWeightMedium });
        for (icon_r, track, icon, v, enabled) in rows {
            if let Some(i) = self.symbol(icon, 13.0, (1.0, 1.0, 1.0, 0.85)) {
                draw_centered(&i, icon_r, a, 1.0);
            }
            fill_round(track, 2.5, &white(0.18 * a));
            if enabled {
                let mut done = track;
                done.size.width *= v;
                fill_round(done, 2.5, &white(0.92 * a));
                let knob = NSPoint::new(track.origin.x + track.size.width * v, track.origin.y + 2.5);
                fill_circle(knob, 8.0, &NSColor::colorWithWhite_alpha(0.0, 0.35 * a));
                fill_circle(knob, 7.0, &white(a));
            }
            draw_text(
                &if enabled { format!("{}%", (v * 100.0).round() as i32) } else { "—".into() },
                rect(w - PAD - 40.0, icon_r.origin.y + 4.0, 40.0, 16.0),
                &pct_font,
                &white(0.6 * a),
                NSTextAlignment::Right,
            );
        }
    }

    /// Spinner (busy), raised hand (needs you) or green check (done).
    fn draw_claude_indicator(&self, c: &status::Session, frame: usize, r: NSRect, size: f64, a: f64) {
        match c.activity {
            status::Activity::Thinking | status::Activity::Tool(_) => {
                let font = NSFont::systemFontOfSize_weight(size, unsafe { NSFontWeightBold });
                let h = size + 4.0;
                draw_text(
                    SPINNER[frame % SPINNER.len()],
                    rect(r.origin.x, r.origin.y + (r.size.height - h) / 2.0, r.size.width, h),
                    &font,
                    &rgb(CLAUDE_ORANGE, a),
                    NSTextAlignment::Center,
                );
            }
            status::Activity::Waiting => {
                let o = CLAUDE_ORANGE;
                if let Some(i) = self.symbol("hand.raised.fill", size - 3.0, (o.0, o.1, o.2, 1.0)) {
                    draw_centered(&i, r, a, 1.0);
                }
            }
            status::Activity::Done => {
                if let Some(i) = self.symbol("checkmark.circle.fill", size - 3.0, (0.3, 0.85, 0.45, 1.0)) {
                    draw_centered(&i, r, a, 1.0);
                }
            }
        }
    }

    /// "􀧓 12%  􀫦 68%  🌡 47°" right-aligned so it ends at `right`.
    fn draw_sysbar(&self, x: &sysinfo::Stats, right: f64, nh: f64, a: f64) {
        let mem = if x.mem_total > 0 { x.mem_used as f64 / x.mem_total as f64 } else { 0.0 };
        let warn = |v: f64, hi: f64, crit: f64| {
            if v >= crit {
                (1.0, 0.4, 0.35)
            } else if v >= hi {
                (1.0, 0.7, 0.3)
            } else {
                (1.0, 1.0, 1.0)
            }
        };
        let mut items: Vec<(&str, String, (f64, f64, f64))> = vec![
            ("cpu", format!("{:.0}%", x.cpu * 100.0), warn(x.cpu, 0.7, 0.9)),
            ("memorychip", format!("{:.0}%", mem * 100.0), warn(mem, 0.8, 0.92)),
        ];
        if let Some(t) = x.temp {
            items.push(("thermometer.medium", format!("{t:.0}°"), warn(t, 70.0, 85.0)));
        }
        let text_font = NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe { NSFontWeightSemibold });
        let item_w = |s: &str| 15.0 + s.chars().count() as f64 * 6.8;
        let gap = 9.0;
        let total: f64 = items.iter().map(|(_, s, _)| item_w(s)).sum::<f64>() + gap * (items.len() as f64 - 1.0);
        let mut cx = right - total;
        for (icon, txt, col) in &items {
            if let Some(i) = self.symbol(icon, 10.0, (col.0, col.1, col.2, 0.7)) {
                draw_centered(&i, rect(cx, 0.0, 12.0, nh), a, 1.0);
            }
            draw_text(txt, rect(cx + 14.0, (nh - 14.0) / 2.0, item_w(txt), 14.0), &text_font, &rgb(*col, 0.9 * a), NSTextAlignment::Left);
            cx += item_w(txt) + gap;
        }
    }

    /// The page menu: small pills in the strip left of the hardware notch.
    fn draw_tabs(&self, f: &Frame, a: f64) {
        for (page, r) in tab_rects(f.geo.notch_h) {
            let selected = f.page == page;
            let hovered = f.hover == Some(Btn::Tab(page));
            let bg = if selected { 0.18 } else if hovered { 0.09 } else { 0.0 };
            if bg > 0.0 {
                fill_round(r, r.size.height / 2.0, &white(bg * a));
            }
            let name = PAGES.iter().find(|(p, _)| *p == page).map(|(_, n)| *n).unwrap_or("circle");
            let col = match (selected, page) {
                (true, Page::Claude) => (CLAUDE_ORANGE.0, CLAUDE_ORANGE.1, CLAUDE_ORANGE.2, 1.0),
                (true, _) => (1.0, 1.0, 1.0, 1.0),
                (false, _) => (1.0, 1.0, 1.0, 0.45),
            };
            let claude = f.sessions.first().filter(|_| page == Page::Claude);
            match claude {
                Some(c) if c.busy() => {
                    let font = NSFont::systemFontOfSize_weight(14.0, unsafe { NSFontWeightBold });
                    draw_text(
                        SPINNER[f.spin_frame % SPINNER.len()],
                        rect(r.origin.x, r.origin.y + (r.size.height - 18.0) / 2.0, r.size.width, 18.0),
                        &font,
                        &rgb(CLAUDE_ORANGE, a),
                        NSTextAlignment::Center,
                    );
                }
                Some(c) => self.draw_claude_indicator(c, f.spin_frame, r, 13.0, a),
                None => {
                    if let Some(i) = self.symbol(name, 11.5, col) {
                        draw_centered(&i, r, a, 1.0);
                    }
                }
            }
        }
    }

    /// Claude usage page, like Claude Code's `/usage`: two bars (current
    /// 5-hour window, this week) and a one-line summary of today.
    fn draw_claude(&self, f: &Frame, w: f64, a: f64) {
        let top = f.geo.notch_h;
        let gray = white(0.5 * a);
        let orange = CLAUDE_ORANGE;
        let semibold = unsafe { NSFontWeightSemibold };
        let medium = unsafe { NSFontWeightMedium };
        let x = PAD;
        let bw = w - 2.0 * PAD;
        let now = chrono::Local::now();
        let limits = match &f.limits {
            Some(Ok(l)) => Some(l),
            _ => None,
        };

        let bar_color = |pct: f64| {
            if pct >= 90.0 {
                (1.0, 0.4, 0.35)
            } else if pct >= 75.0 {
                (1.0, 0.7, 0.3)
            } else {
                orange
            }
        };
        // one row: label ............ value · detail, then a bar
        let row = |y: f64, label: &str, pct: Option<f64>, detail: String| {
            draw_text(label, rect(x, y, 200.0, 18.0), &font(13.5, semibold), &white(a), NSTextAlignment::Left);
            let value = match pct {
                Some(p) => format!("{p:.0}%"),
                None => "—".into(),
            };
            let right = if detail.is_empty() { value.clone() } else { format!("{value}  ·  {detail}") };
            draw_text(&right, rect(x + 200.0, y + 1.0, bw - 200.0, 16.0), &font(12.0, medium), &white(0.7 * a), NSTextAlignment::Right);
            let bar = rect(x, y + 24.0, bw, 8.0);
            fill_round(bar, 4.0, &white(0.13 * a));
            if let Some(p) = pct {
                let frac = (p / 100.0).clamp(0.0, 1.0);
                if frac > 0.0 {
                    let mut done = bar;
                    done.size.width = (bw * frac).max(8.0);
                    fill_round(done, 4.0, &rgb(bar_color(p), a));
                }
            }
        };
        let reset_in = |t: chrono::DateTime<chrono::Local>| {
            let m = (t - now).num_minutes().max(0);
            if m >= 24 * 60 {
                const TH_DAYS: [&str; 7] = ["จ.", "อ.", "พ.", "พฤ.", "ศ.", "ส.", "อา."];
                format!("รีเซ็ต {} {}", TH_DAYS[t.weekday().num_days_from_monday() as usize], t.format("%H:%M"))
            } else {
                format!("รีเซ็ตใน {} ชม. {:02} น.", m / 60, m % 60)
            }
        };

        // ---- 5-hour window ----
        let five = limits.and_then(|l| l.five_hour.as_ref());
        let five_reset = five
            .and_then(|l| l.resets_at)
            .or_else(|| f.usage.as_ref().and_then(|u| u.block.as_ref()).map(|b| b.end));
        row(top + 14.0, "รอบ 5 ชม.", five.map(|l| l.pct), five_reset.map(reset_in).unwrap_or_default());

        // ---- this week ----
        let week = limits.and_then(|l| l.week.as_ref());
        row(top + 60.0, "สัปดาห์นี้", week.map(|l| l.pct), week.and_then(|l| l.resets_at).map(reset_in).unwrap_or_default());

        // ---- today, one line ----
        let y = top + 110.0;
        let mut line = String::new();
        if let Some(u) = &f.usage {
            line = format!("วันนี้ {} tokens · {} ข้อความ", claude::format_tokens(u.today.total()), u.today_messages);
            if let Some(m) = &u.top_model {
                line.push_str(&format!(" · {m}"));
            }
        }
        if let Some(Err(e)) = &f.limits {
            // can't show % — say why, briefly
            line = if line.is_empty() { e.clone() } else { format!("{line}    ({e})") };
        }
        fill_circle(NSPoint::new(x + 3.5, y + 8.0), 3.5, &rgb(orange, a));
        draw_text(&line, rect(x + 12.0, y, bw - 12.0, 16.0), &font(11.5, 0.0), &gray, NSTextAlignment::Left);
        // ask capsule
        let pill = ask_pill(w, top);
        fill_round(pill, pill.size.height / 2.0, &white(0.08 * a));
        white(0.12 * a).setStroke();
        let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(pill, pill.size.height / 2.0, pill.size.height / 2.0);
        outline.setLineWidth(1.0);
        outline.stroke();
        if let Some(i) = self.symbol("sparkle", 13.0, (orange.0, orange.1, orange.2, 1.0)) {
            draw_centered(&i, rect(pill.origin.x + 8.0, pill.origin.y, 24.0, pill.size.height), a, 1.0);
        }
        let send = ask_send_rect(w, top);
        fill_circle(center(send), 14.0, &rgb(orange, a));
        if let Some(i) = self.symbol("arrow.up", 12.0, (1.0, 1.0, 1.0, 1.0)) {
            draw_centered(&i, send, a, 1.0);
        }
        if f.ask_open {
            let r = rect(x, top + 186.0, bw, CLAUDE_CHAT_H - 186.0 - 14.0);
            fill_round(r, 14.0, &white(0.05 * a));
            draw_text("esc  เริ่มใหม่", rect(r.origin.x, r.origin.y + 8.0, r.size.width - 14.0, 13.0), &font(10.0, 0.0), &white(0.3 * a), NSTextAlignment::Right);
        }
    }

    /// Big round play/pause button: white disc, accent halo that breathes
    /// while playing, and the glyph morphs (scale + cross-fade) on change.
    fn draw_play_button(&self, f: &Frame, r: NSRect, playing: bool, press: f64, a: f64) {
        let c = center(r);
        let hovered = f.hover == Some(Btn::Play);
        let s = press * if hovered { 1.07 } else { 1.0 };
        let radius = 21.0 * s;

        // halo
        let breathe = if playing { 1.0 } else { 0.0 }; // steady glow, no pulsing
        let halo = (if playing { 0.45 } else { 0.0 }) + if hovered { 0.25 } else { 0.0 };
        if halo > 0.0 {
            if let Some(g) = NSGradient::initWithStartingColor_endingColor(
                NSGradient::alloc(),
                &rgb(f.accent, halo * a),
                &rgb(f.accent, 0.0),
            ) {
                g.drawFromCenter_radius_toCenter_radius_options(
                    c,
                    radius - 1.0,
                    c,
                    radius + 14.0 + 3.0 * breathe,
                    NSGradientDrawingOptions::empty(),
                );
            }
        }

        // disc with a soft top highlight
        fill_circle(c, radius, &white(a));
        if let Some(g) = NSGradient::initWithStartingColor_endingColor(
            NSGradient::alloc(),
            &white(0.0),
            &NSColor::colorWithWhite_alpha(0.0, 0.10 * a),
        ) {
            let disc = NSBezierPath::bezierPathWithOvalInRect(rect(c.x - radius, c.y - radius, 2.0 * radius, 2.0 * radius));
            g.drawInBezierPath_angle(&disc, 90.0);
        }

        // glyph morph
        let glyph = |p: bool| if p { "pause.fill" } else { "play.fill" };
        let draw_glyph = |p: bool, alpha: f64, scale: f64| {
            if let Some(i) = self.symbol(glyph(p), 18.0, (0.0, 0.0, 0.0, 1.0)) {
                let mut gr = r;
                if !p {
                    gr.origin.x += 1.5; // optical centring of ▶
                }
                draw_centered(&i, gr, alpha * a, scale * s);
            }
        };
        match f.glyph_changed {
            Some((old, at)) if at.elapsed().as_secs_f64() < 0.25 => {
                let e = at.elapsed().as_secs_f64() / 0.25;
                let e = 1.0 - (1.0 - e).powi(3); // ease-out
                draw_glyph(old, 1.0 - e, 1.0 - 0.5 * e);
                draw_glyph(playing, e, 0.5 + 0.5 * e);
            }
            _ => draw_glyph(playing, 1.0, 1.0),
        }
    }

    fn draw_expanded(&self, f: &Frame, l: &Layout, w: f64, a: f64) {
        let Some(tr) = &f.track else {
            // ----- nothing playing -----
            let cy = f.geo.notch_h + MUSIC_BODY_H / 2.0;
            if let Some(i) = self.symbol("music.note", 22.0, (1.0, 1.0, 1.0, 0.35)) {
                draw_centered(&i, rect(0.0, cy - 36.0, w, 30.0), a, 1.0);
            }
            draw_text(
                "ยังไม่มีเพลงเล่นอยู่",
                rect(0.0, cy, w, 20.0),
                &font(14.0, unsafe { NSFontWeightSemibold }),
                &white(0.75 * a),
                NSTextAlignment::Center,
            );
            draw_text(
                "Spotify · Apple Music · YouTube",
                rect(0.0, cy + 21.0, w, 18.0),
                &font(12.0, 0.0),
                &white(0.4 * a),
                NSTextAlignment::Center,
            );
            return;
        };

        // ----- artwork + app badge -----
        match &f.art {
            Some(img) => draw_artwork(img, l.art, 16.0, a),
            None => {
                fill_round(l.art, 16.0, &white(0.08 * a));
                if let Some(i) = self.symbol("music.note", 30.0, (1.0, 1.0, 1.0, 0.5)) {
                    draw_centered(&i, l.art, a, 1.0);
                }
            }
        }
        if let Some(icon) = &f.icon {
            let s = 26.0;
            let c = NSPoint::new(l.art.origin.x + l.art.size.width - 6.0, l.art.origin.y + l.art.size.height - 6.0);
            fill_circle(c, s / 2.0 + 1.5, &NSColor::colorWithWhite_alpha(0.0, a));
            draw_image(icon, rect(c.x - s / 2.0, c.y - s / 2.0, s, s), NSRect::ZERO, a);
        }

        // ----- text -----
        draw_text(
            &tr.title,
            l.title,
            &font(16.0, unsafe { NSFontWeightBold }),
            &white(a),
            NSTextAlignment::Left,
        );
        draw_text(
            if tr.artist.is_empty() { "—" } else { &tr.artist },
            l.artist,
            &font(13.0, unsafe { NSFontWeightMedium }),
            &white(0.55 * a),
            NSTextAlignment::Left,
        );
        draw_eq(l.eq, f.accent, a, tr.playing, f.eq);

        // ----- controls -----
        let press_scale = |b: Btn| match f.pressed {
            Some((p, t)) if p == b => {
                let e = t.elapsed().as_secs_f64() / 0.35;
                if e < 1.0 { 1.0 - 0.18 * (e * PI).sin() } else { 1.0 }
            }
            _ => 1.0,
        };
        for (btn, r, name) in [
            (Btn::Prev, l.prev, "backward.fill"),
            (Btn::Next, l.next, "forward.fill"),
        ] {
            if f.hover == Some(btn) {
                fill_circle(center(r), 20.0 * press_scale(btn), &white(0.12 * a));
            }
            if let Some(i) = self.symbol(name, 19.0, (1.0, 1.0, 1.0, 1.0)) {
                draw_centered(&i, r, a, press_scale(btn));
            }
        }
        self.draw_play_button(f, l.play, tr.playing, press_scale(Btn::Play), a);

        // ----- player volume: 🔈 ━━━━━━──── 🔊 -----
        if let Some(vol) = f.vol_drag.or(tr.volume).filter(|_| tr.full) {
            let lo = if vol <= 0.001 { "speaker.slash.fill" } else { "speaker.fill" };
            if let Some(i) = self.symbol(lo, 11.0, (1.0, 1.0, 1.0, 0.6)) {
                draw_centered(&i, l.vol_icon, a, 1.0);
            }
            if let Some(i) = self.symbol("speaker.wave.3.fill", 11.0, (1.0, 1.0, 1.0, 0.6)) {
                draw_centered(&i, l.vol_max, a, 1.0);
            }
            let active = f.vol_drag.is_some() || f.vol_hover;
            let mut track = l.vol_track;
            if active {
                track.origin.y -= 1.5;
                track.size.height += 3.0;
            }
            let r = track.size.height / 2.0;
            fill_round(track, r, &white(0.2 * a));
            if vol > 0.0 {
                let mut done = track;
                done.size.width = (track.size.width * vol).max(track.size.height);
                fill_round(done, r, &white(if active { 1.0 } else { 0.8 } * a));
            }
            if active {
                let knob = NSPoint::new(track.origin.x + track.size.width * vol, track.origin.y + r);
                fill_circle(knob, 7.5, &NSColor::colorWithWhite_alpha(0.0, 0.3 * a));
                fill_circle(knob, 6.5, &white(a));
            }
        }

        // ----- progress / hint -----
        if tr.full && f.dur > 0.0 {
            let frac = f.seek_drag.unwrap_or((f.pos / f.dur).clamp(0.0, 1.0));
            let active = f.seek_drag.is_some() || f.bar_hover;
            let mut bar = l.bar;
            if active {
                bar.origin.y -= 1.5;
                bar.size.height += 3.0;
            }
            let r = bar.size.height / 2.0;
            fill_round(bar, r, &white(0.18 * a));
            let mut done = bar;
            done.size.width = (done.size.width * frac).max(bar.size.height);
            fill_round(done, r, &rgb(f.accent, a));
            if active {
                let knob = NSPoint::new(bar.origin.x + bar.size.width * frac, bar.origin.y + r);
                fill_circle(knob, 7.5, &NSColor::colorWithWhite_alpha(0.0, 0.3 * a));
                fill_circle(knob, 6.5, &white(a));
            }
            let f_pos = frac * f.dur;
            let small = NSFont::monospacedDigitSystemFontOfSize_weight(10.5, unsafe { NSFontWeightMedium });
            let ty = l.bar.origin.y + l.bar.size.height + 6.0;
            let col = white(0.55 * a);
            let hi = if f.seek_drag.is_some() { white(a) } else { col.clone() };
            draw_text(&fmt_time(f_pos), rect(l.bar.origin.x, ty, 60.0, 13.0), &small, &hi, NSTextAlignment::Left);
            draw_text(
                &format!("-{}", fmt_time(f.dur - f_pos)),
                rect(l.bar.origin.x + l.bar.size.width - 60.0, ty, 60.0, 13.0),
                &small,
                &hi,
                NSTextAlignment::Right,
            );
        } else if !tr.full {
            let name = if tr.app == App::Safari { "Safari" } else { "Chrome" };
            fill_round(l.bar, 2.0, &white(0.12 * a));
            draw_text(
                &format!("เปิดอนุญาต JavaScript ใน {name} เพื่อดูเวลาและเลื่อนเพลง"),
                rect(l.bar.origin.x, l.bar.origin.y + 10.0, l.bar.size.width, 14.0),
                &font(10.5, 0.0),
                &white(0.4 * a),
                NSTextAlignment::Center,
            );
        } else {
            fill_round(l.bar, 2.0, &white(0.18 * a)); // live stream: no duration
            draw_text("LIVE", rect(l.bar.origin.x, l.bar.origin.y + 10.0, 60.0, 13.0), &font(10.0, unsafe { NSFontWeightBold }), &rgb((1.0, 0.35, 0.35), a), NSTextAlignment::Left);
        }
    }
}

/// Find the built-in display with a notch (falls back to the main screen).
fn detect_geometry(mtm: MainThreadMarker) -> Geometry {
    let screens = NSScreen::screens(mtm);
    let notched = screens.iter().find(|s| s.safeAreaInsets().top > 0.0);
    let screen = notched
        .or_else(|| NSScreen::mainScreen(mtm))
        .expect("no screen");
    let frame = screen.frame();
    let top = screen.safeAreaInsets().top;
    if top > 0.0 {
        let l = screen.auxiliaryTopLeftArea();
        let r = screen.auxiliaryTopRightArea();
        let notch_w = frame.size.width - l.size.width - r.size.width;
        Geometry { screen: frame, notch_w, notch_h: top }
    } else {
        // No notch: draw a fake one in the menu bar.
        let mb = frame.origin.y + frame.size.height
            - (screen.visibleFrame().origin.y + screen.visibleFrame().size.height);
        Geometry { screen: frame, notch_w: 190.0, notch_h: if mb > 0.0 { mb } else { 32.0 } }
    }
}

/// Generated cover art for demo screenshots (soft gradient + rings).
fn demo_artwork() -> Retained<NSImage> {
    let size = 300.0;
    let img = NSImage::initWithSize(NSImage::alloc(), NSSize::new(size, size));
    #[allow(deprecated)]
    img.lockFocus();
    let colors = NSArray::from_retained_slice(&[
        rgb((0.98, 0.42, 0.45), 1.0),
        rgb((0.62, 0.3, 0.85), 1.0),
        rgb((0.18, 0.2, 0.55), 1.0),
    ]);
    if let Some(g) = NSGradient::initWithColors(NSGradient::alloc(), &colors) {
        g.drawInRect_angle(rect(0.0, 0.0, size, size), 45.0);
    }
    for (r, alpha) in [(120.0, 0.18), (85.0, 0.22), (50.0, 0.3)] {
        let p = NSBezierPath::bezierPathWithOvalInRect(rect(size / 2.0 - r, size / 2.0 - r, 2.0 * r, 2.0 * r));
        p.setLineWidth(6.0);
        white(alpha).setStroke();
        p.stroke();
    }
    #[allow(deprecated)]
    img.unlockFocus();
    img
}

/// `notch --snapshot DIR`: render the collapsed + expanded island with the
/// current track to PNGs (handy for checking the design without a screen grab).
fn snapshot(mtm: MainThreadMarker, dir: &str) {
    // NOTCH_DEMO=1: made-up track, generated artwork and sample numbers —
    // for README screenshots (no copyrighted cover art, no personal data).
    let demo = std::env::var_os("NOTCH_DEMO").is_some();
    let (cmd_tx, _cmd_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    let track = if demo {
        Some(Track {
            app: App::Spotify,
            playing: true,
            title: "Midnight Drive".into(),
            artist: "The Placeholders".into(),
            id: "demo".into(),
            position: 85.0,
            duration: 214.0,
            artwork_url: None,
            tab: None,
            full: true,
            volume: Some(0.65),
        })
    } else {
        player::current()
    };
    let art = if demo { None } else { track.as_ref().and_then(player::fetch_artwork) };
    msg_tx.send(Msg::Track(track)).unwrap();
    msg_tx.send(Msg::Artwork(art)).unwrap();
    let (usage_tx, usage_rx) = mpsc::channel();
    usage_tx
        .send(if demo {
            let mut u = claude::Usage::default();
            u.today.input = 1_200;
            u.today.output = 310_000;
            u.today.cache_write = 1_400_000;
            u.today.cache_read = 46_000_000;
            u.today_messages = 182;
            u.top_model = Some("Opus 5.5".into());
            u
        } else {
            claude::current()
        })
        .unwrap();
    let (status_tx, status_rx) = mpsc::channel();
    status_tx
        .send(if demo {
            vec![status::Session { project: "notch".into(), activity: status::Activity::Tool("Bash".into()), since: status::unix_now() - 42.0 }]
        } else {
            status::current()
        })
        .unwrap();
    let (sys_tx, sys_rx) = mpsc::channel();
    let _ = sys_tx.send(sysinfo::current());
    let (limits_tx, limits_rx) = mpsc::channel();
    let (limits_poke, _poke_rx) = mpsc::channel();
    let (ask_tx, _ask_q_rx) = mpsc::channel();
    let (_ask_a_tx, ask_rx) = mpsc::channel();
    // sample data for the render (no network / keychain access)
    let _ = limits_tx.send(Ok(claude_api::Limits {
        five_hour: Some(claude_api::Limit { pct: 23.0, resets_at: Some(chrono::Local::now() + chrono::TimeDelta::minutes(111)) }),
        week: Some(claude_api::Limit { pct: 42.0, resets_at: Some(chrono::Local::now() + chrono::TimeDelta::days(3)) }),
    }));
    let (privacy_tx, privacy_rx) = mpsc::channel();
    // sample data for the render
    privacy_tx
        .send(privacy::Privacy { camera: true })
        .unwrap();
    let (controls_tx, controls_rx) = mpsc::channel();
    let (action_tx, _action_rx) = mpsc::channel();
    controls_tx.send(controls::read()).unwrap();
    let (req_tx, _req_rx) = mpsc::channel();
    let (_resp_tx, resp_rx) = mpsc::channel();
    let (wx_tx, _wx_req_rx) = mpsc::channel();
    let (wx_resp_tx, wx_rx) = mpsc::channel();
    // sample data for the render (generic city centre, not your location)
    let _ = wx_resp_tx.send(weather::fetch(13.75, 100.50));
    let view = NotchView::new(
        mtm,
        new_state(detect_geometry(mtm), cmd_tx, msg_rx, usage_rx, status_rx, controls_rx, action_tx, req_tx, resp_rx, wx_tx, wx_rx, privacy_rx, sys_rx, limits_rx, limits_poke, ask_tx, ask_rx),
    );
    view.tick();
    if demo {
        let art = demo_artwork();
        let mut st = view.ivars().state.borrow_mut();
        st.accent = accent_from(&art).unwrap_or(DEFAULT_ACCENT);
        st.art_blur = blurred(&art);
        st.art = Some(art);
        st.app_icon = app_icon(App::Spotify).or_else(|| app_icon(App::Music)).map(|i| (App::Spotify, i));
        st.eq_levels = [0.55, 0.95, 0.7, 0.4];
        st.spin_frame = 4; // ✻
        if let Some(x) = st.sys.as_mut() {
            x.cpu = 0.18;
        }
    }
    for (name, expanded, hover, playing, page) in [
        ("collapsed", false, None, false, Page::Music),
        ("expanded-paused", true, None, false, Page::Music),
        ("expanded-playing", true, None, true, Page::Music),
        ("expanded-hover", true, Some(Btn::Play), true, Page::Music),
        ("claude", true, Some(Btn::Tab(Page::Music)), true, Page::Claude),
        ("controls", true, None, true, Page::Controls),
        ("charging", false, None, true, Page::Music),
        ("privacy", false, None, true, Page::Music),
        ("idle", false, None, false, Page::Music),
        ("lowbattery", false, None, true, Page::Music),
        ("volume", false, None, true, Page::Music),
        ("brightness", false, None, true, Page::Music),
        ("drop", true, None, true, Page::Music),
        ("weather", true, None, true, Page::Weather),
        ("camera", true, None, true, Page::Camera),
        ("shelf", true, None, true, Page::Shelf),
        ("ask", true, None, true, Page::Claude),
        ("wifi", true, None, true, Page::Controls),
        ("bluetooth", true, None, true, Page::Controls),
    ] {
        {
            let mut st = view.ivars().state.borrow_mut();
            st.page = page;
            st.page_changed = Instant::now() - std::time::Duration::from_secs(1);
            st.drop_hover = name == "drop";
            st.drop_to_shelf = true;
            if name == "ask" {
                st.ask_q = Some("สรุปการเปลี่ยนแปลงใน Rust 2024 edition ให้หน่อย".into());
                st.ask_answer = Some("Rust 2024 edition มีการเปลี่ยนแปลงหลักๆ ดังนี้:\n\n1. **RPIT lifetime capture** — `impl Trait` ในตำแหน่ง return จะ capture lifetime ทั้งหมดโดยอัตโนมัติ\n2. **`unsafe extern` blocks** — ต้องเขียน `unsafe extern \"C\" { … }`\n3. **`gen` เป็น reserved keyword**\n4. **Never type fallback** เปลี่ยนเป็น `!`\n\nส่วนใหญ่ย้ายได้อัตโนมัติด้วย `cargo fix --edition`".into());
            } else {
                st.ask_q = None;
                st.ask_answer = None;
            }
            st.expanded_at = Instant::now() - std::time::Duration::from_secs(1);
            if name == "shelf" && st.shelf.is_empty() {
                st.shelf = ["Documents", "Downloads", "Desktop"]
                    .iter()
                    .filter_map(|d| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(d)))
                    .collect();
            }
            st.hovering = false;
            st.detail = match name {
                "wifi" => Some(Detail::Wifi),
                "bluetooth" => Some(Detail::Bluetooth),
                _ => None,
            };
            if name == "wifi" {
                let net = |ssid: &str, rssi, secure, known, current| nets::Net { ssid: ssid.into(), rssi, secure, known, current };
                st.nets = vec![
                    net("Home 5G", -48, true, true, true),
                    net("Office", -62, true, true, false),
                    net("Starbucks Free WiFi", -71, false, false, false),
                    net("TrueMove H", -77, true, false, false),
                    net("AIS SUPER WiFi", -83, true, false, false),
                    net("Neighbor", -88, true, false, false),
                ];
                st.joining = Some("Office".into());
                st.password_for = Some("TrueMove H".into());
            }
            if name == "bluetooth" {
                let dev = |n: &str, a: &str, c| nets::BtDev { name: n.into(), addr: a.into(), connected: c };
                st.bt_devs = vec![
                    dev("AirPods Pro", "a", true),
                    dev("Magic Keyboard", "b", true),
                    dev("iPad (6)", "c", false),
                    dev("JBL Flip 6", "d", false),
                ];
            }
            st.wings = name != "idle";
            if name == "idle" {
                st.privacy = privacy::Privacy::default();
                st.sessions.clear();
            }
            if name == "weather" {
                st.place = Some("กรุงเทพมหานคร".into());
            }
            let ago = |ms| Instant::now() - std::time::Duration::from_millis(ms);
            st.banner = match name {
                "charging" => Some((Banner::Charging(87), ago(1500), ago(1500))),
                "lowbattery" => Some((Banner::LowBattery(15), ago(800), ago(800))),
                "volume" => Some((Banner::Volume(0.62, false), ago(800), ago(200))),
                "brightness" => Some((Banner::Brightness(0.35), ago(800), ago(200))),
                _ => None,
            };
            st.expanded = expanded;
            st.hover = hover;
            if let Some(t) = st.track.as_mut() {
                if t.full && t.volume.is_none() {
                    t.volume = Some(0.65);
                }
                t.playing = playing;
            }
        }
        let f = view.target_frame();
        view.setFrame(rect(0.0, 0.0, f.size.width, f.size.height));
        view.tick_ask(); // real subviews (text field / answer) for this page
        let b = view.bounds();
        let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(b) else { continue };
        view.cacheDisplayInRect_toBitmapImageRep(b, &rep);
        let data = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        };
        if let Some(d) = data {
            let path = format!("{dir}/{name}.png");
            std::fs::write(&path, d.to_vec()).unwrap();
            println!("wrote {path}");
        }
    }
}

fn new_state(
    geo: Geometry,
    cmd_tx: Sender<Cmd>,
    msg_rx: Receiver<Msg>,
    usage_rx: Receiver<claude::Usage>,
    status_rx: Receiver<Vec<status::Session>>,
    controls_rx: Receiver<controls::Controls>,
    action_tx: Sender<controls::Action>,
    req_tx: Sender<nets::Req>,
    resp_rx: Receiver<nets::Resp>,
    wx_tx: Sender<(f64, f64)>,
    wx_rx: Receiver<Result<weather::Weather, String>>,
    privacy_rx: Receiver<privacy::Privacy>,
    sys_rx: Receiver<sysinfo::Stats>,
    limits_rx: Receiver<Result<claude_api::Limits, String>>,
    limits_poke: Sender<()>,
    ask_tx: Sender<ask::Question>,
    ask_rx: Receiver<Result<ask::Answer, String>>,
) -> State {
    State {
        geo,
        page: Page::Music,
        page_changed: Instant::now(),
        usage: None,
        usage_rx,
        sessions: Vec::new(),
        status_rx,
        spin_frame: 0,
        controls: None,
        controls_rx,
        action_tx,
        controls_touched: None,
        dragging: None,
        click_count: 1,
        vol_drag: None,
        vol_target: None,
        vol_hover: false,
        vol_before_mute: 0.6,
        art_blur: None,
        seek_drag: None,
        seek_target: None,
        bar_hover: false,
        detail: None,
        nets: Vec::new(),
        wifi_err: None,
        scanning: false,
        next_scan: None,
        joining: None,
        join_failed: None,
        password_for: None,
        pw_field: None,
        bt_devs: Vec::new(),
        bt_busy: None,
        list_scroll: 0.0,
        req_tx,
        resp_rx,
        location: None,
        weather: None,
        weather_err: None,
        weather_at: None,
        place: None,
        coords: None,
        locating: false,
        wx_tx,
        wx_rx,
        geocoder: None,
        cam_session: None,
        cam_view: None,
        cam_on: false,
        cam_denied: false,
        cam_asked: false,
        pending_tile: None,
        drop_hover: false,
        wings: false,
        sys: None,
        sys_rx,
        ask_tx,
        ask_rx,
        ask_session: None,
        ask_q: None,
        ask_answer: None,
        ask_pending: false,
        ask_field: None,
        ask_view: None,
        shelf: shelf::load(),
        drop_to_shelf: false,
        shelf_press: None,
        shelf_hover: None,
        shelf_dragging: None,
        limits: None,
        limits_rx,
        limits_poke,
        limits_poked: None,
        tap: None,
        tap_failed_at: None,
        tap_idle_since: None,
        tap_heard: false,
        eq_levels: [0.0; 4],
        eq_peak: [1e-3; 4],
        hovering: false,
        privacy: privacy::Privacy::default(),
        privacy_rx,
        last_active: Instant::now() - std::time::Duration::from_secs(3600),
        banner: None,
        last_volume: audio::output_volume(),
        last_brightness: controls::brightness(),
        hud_poll: 0,
        low_warned: 100,
        on_ac: power::on_ac(),
        expanded: false,
        track: None,
        track_at: Instant::now(),
        art: None,
        accent: DEFAULT_ACCENT,
        app_icon: None,
        hover: None,
        pressed: None,
        expanded_at: Instant::now(),
        optimistic: None,
        shown_playing: false,
        glyph_changed: None,
        symbols: HashMap::new(),
        started: Instant::now(),
        cmd_tx,
        msg_rx,
        window: None,
    }
}

fn main() {
    // `notch --hook <Event>`: called by Claude Code hooks (see status.rs).
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--hook") {
        status::handle_hook(args.get(i + 1).map(String::as_str).unwrap_or(""));
        return;
    }
    if std::env::args().any(|a| a == "--scan") {
        nets::debug_print();
        return;
    }
    // `notch --dump`: print what is currently detected and exit (debugging).
    if std::env::args().any(|a| a == "--dump") {
        println!("{:#?}", player::current());
        println!("{:#?}", claude::current());
        println!("{:#?}", status::current());
        return;
    }

    let mtm = MainThreadMarker::new().unwrap();
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--snapshot") {
        return snapshot(mtm, args.get(i + 1).map(String::as_str).unwrap_or("."));
    }
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    std::thread::spawn(move || player::run(msg_tx, cmd_rx));
    let (usage_tx, usage_rx) = mpsc::channel();
    std::thread::spawn(move || claude::run(usage_tx));
    let (status_tx, status_rx) = mpsc::channel();
    std::thread::spawn(move || status::run(status_tx));
    let (privacy_tx, privacy_rx) = mpsc::channel();
    std::thread::spawn(move || privacy::run(privacy_tx));
    let (sys_tx, sys_rx) = mpsc::channel();
    std::thread::spawn(move || sysinfo::run(sys_tx));
    let (limits_tx, limits_rx) = mpsc::channel();
    let (limits_poke, poke_rx) = mpsc::channel();
    std::thread::spawn(move || claude_api::run(limits_tx, poke_rx));
    let (ask_tx, ask_q_rx) = mpsc::channel();
    let (ask_a_tx, ask_rx) = mpsc::channel();
    std::thread::spawn(move || ask::run(ask_q_rx, ask_a_tx));
    let (controls_tx, controls_rx) = mpsc::channel();
    let (action_tx, action_rx) = mpsc::channel();
    std::thread::spawn(move || controls::run_loop(controls_tx, action_rx));
    let (req_tx, req_rx) = mpsc::channel();
    let (resp_tx, resp_rx) = mpsc::channel();
    std::thread::spawn(move || nets::run(req_rx, resp_tx));
    let (wx_tx, wx_req_rx) = mpsc::channel();
    let (wx_resp_tx, wx_rx) = mpsc::channel();
    std::thread::spawn(move || weather::run(wx_req_rx, wx_resp_tx));

    let view = NotchView::new(
        mtm,
        new_state(detect_geometry(mtm), cmd_tx, msg_rx, usage_rx, status_rx, controls_rx, action_tx, req_tx, resp_rx, wx_tx, wx_rx, privacy_rx, sys_rx, limits_rx, limits_poke, ask_tx, ask_rx),
    );

    let panel: Retained<KeyPanel> = unsafe {
        msg_send![
            KeyPanel::alloc(mtm),
            initWithContentRect: view.target_frame(),
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    let panel: Retained<NSPanel> = Retained::into_super(panel);
    // Location permission → Wi-Fi network names (only works inside Notch.app)
    view.ivars().state.borrow_mut().location = Some(unsafe { CLLocationManager::new() });
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(26); // above the menu bar (NSMainMenuWindowLevel = 24)
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setMovable(false);
    panel.setHidesOnDeactivate(false);
    panel.setAcceptsMouseMovedEvents(true);
    panel.setContentView(Some(&view));
    view.ivars().state.borrow_mut().window = Some(panel.clone());
    panel.orderFrontRegardless();

    power::watch();

    // Click anywhere outside the island (in another app, on the desktop, …)
    // → close it. A global monitor only sees events aimed at *other* apps, so
    // clicks on the island itself still go to `mouseDown:`. Mouse monitors
    // don't need any extra permission.
    let v = view.clone();
    let outside_click = RcBlock::new(move |_e: NonNull<NSEvent>| {
        if v.ivars().state.borrow().expanded {
            v.set_expanded(false);
        }
    });
    let _click_monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown,
        &outside_click,
    );

    // Main-thread timer (~30 fps): pull updates from the player thread and
    // drive the little animations.
    let v = view.clone();
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| v.tick());
    let _timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 30.0, true, &block) };

    app.run();
}
