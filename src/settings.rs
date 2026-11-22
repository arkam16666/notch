//! The settings window, inside Notch itself: opened from the right-click
//! menu, or by opening Notch.app again (Launchpad / Spotlight / Finder)
//! while it's already running. While it's open Notch shows a Dock icon so
//! the window can be found with ⌘-Tab; closing it hides the icon again.
//!
//! Every change is written straight to `~/.claude/notch/config.json`; the
//! notch picks it up within a second, so there's no "Save" button.

use std::cell::RefCell;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, ClassType, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply, NSBackingStoreType, NSBox, NSBoxType,
    NSButton, NSColor, NSColorSpace, NSColorWell, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSFontWeightBold,
    NSFontWeightSemibold, NSMenu, NSMenuItem, NSPopUpButton, NSTextField, NSView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::config::{self, Config, BOOL_KEYS};

/// Popup choices for "fold after music stops".
const IDLE_CHOICES: [(u32, &str); 6] =
    [(3, "3 วินาที"), (5, "5 วินาที"), (10, "10 วินาที"), (30, "30 วินาที"), (60, "1 นาที"), (0, "ไม่หุบ")];
const TAG_LOGIN: isize = 100;
const TAG_COLOR_WELL: isize = 200;
/// Border colour styles (config value, menu title).
/// Where the glow goes (config value, menu title).
const BORDER_SCOPES: [(&str, &str); 2] = [("notch", "รอบ notch"), ("screen", "ขอบจอทั้งจอ")];
const BORDER_STYLES: [(&str, &str); 3] = [("rainbow", "รุ้ง RGB (หมุน)"), ("artwork", "ตามสีปกเพลง"), ("solid", "สีเดียว")];

pub struct Ivars {
    cfg: RefCell<Config>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    // Notch.app opened again while running → show the settings window
    unsafe impl NSApplicationDelegate for Target {
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            show(self.mtm());
            false
        }

        /// ⌘Q / "Quit" in the Dock while the settings window is open only
        /// closes the settings — the notch keeps running. Real quitting goes
        /// through `quit_app` (right-click menu on the notch).
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            let really = REALLY_QUIT.load(std::sync::atomic::Ordering::SeqCst);
            let open = WINDOW.with(|w| w.borrow().clone());
            match open {
                Some(w) if !really && NSApplication::sharedApplication(self.mtm()).isActive() => {
                    w.performClose(None);
                    NSApplicationTerminateReply::TerminateCancel
                }
                _ => NSApplicationTerminateReply::TerminateNow,
            }
        }
    }

    unsafe impl NSWindowDelegate for Target {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            WINDOW.with(|w| w.borrow_mut().take());
            // back to a menu-bar-less, Dock-less accessory app
            NSApplication::sharedApplication(self.mtm()).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }

    impl Target {
        #[unsafe(method(toggle:))]
        fn toggle(&self, sender: &NSButton) {
            let on = sender.state() == NSControlStateValueOn;
            let tag = sender.tag();
            if tag == TAG_LOGIN {
                if let Err(e) = set_login_item(on) {
                    eprintln!("notch: login item: {e}");
                    sender.setState(if on { NSControlStateValueOff } else { NSControlStateValueOn });
                }
                return;
            }
            if let Some(key) = BOOL_KEYS.get(tag as usize) {
                let mut c = self.ivars().cfg.borrow_mut();
                c.set_bool(key, on);
                save(&c);
            }
        }

        #[unsafe(method(idle:))]
        fn idle(&self, sender: &NSPopUpButton) {
            if let Some((secs, _)) = IDLE_CHOICES.get(sender.indexOfSelectedItem() as usize) {
                let mut c = self.ivars().cfg.borrow_mut();
                c.idle_secs = *secs;
                save(&c);
            }
        }

        #[unsafe(method(reset:))]
        fn reset(&self, _sender: &AnyObject) {
            *self.ivars().cfg.borrow_mut() = Config::default();
            save(&Config::default());
            // rebuild the window in place so every control shows the defaults
            let mtm = self.mtm();
            let old = WINDOW.with(|w| w.borrow_mut().take());
            if let Some(old) = old {
                let frame = old.frame();
                old.setDelegate(None);
                old.orderOut(None);
                let new = build_window(self, mtm);
                new.setDelegate(Some(ProtocolObject::from_ref(self)));
                new.setFrameTopLeftPoint(NSPoint::new(frame.origin.x, frame.origin.y + frame.size.height));
                new.makeKeyAndOrderFront(None);
                WINDOW.with(|w| *w.borrow_mut() = Some(new));
            }
        }

        #[unsafe(method(borderStyle:))]
        fn border_style(&self, sender: &NSPopUpButton) {
            if let Some((style, _)) = BORDER_STYLES.get(sender.indexOfSelectedItem() as usize) {
                let mut c = self.ivars().cfg.borrow_mut();
                c.border_style = style.to_string();
                save(&c);
                // the colour well only matters for "สีเดียว"
                if let Some(w) = WINDOW.with(|w| w.borrow().clone()) {
                    if let Some(v) = w.contentView().and_then(|cv| cv.viewWithTag(TAG_COLOR_WELL)) {
                        if let Ok(well) = v.downcast::<NSColorWell>() {
                            well.setEnabled(*style == "solid");
                        }
                    }
                }
            }
        }

        #[unsafe(method(borderScope:))]
        fn border_scope(&self, sender: &NSPopUpButton) {
            if let Some((scope, _)) = BORDER_SCOPES.get(sender.indexOfSelectedItem() as usize) {
                let mut c = self.ivars().cfg.borrow_mut();
                c.border_scope = scope.to_string();
                save(&c);
            }
        }

        #[unsafe(method(borderColor:))]
        fn border_color(&self, sender: &NSColorWell) {
            let Some(c) = sender.color().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) else { return };
            let mut cfg = self.ivars().cfg.borrow_mut();
            cfg.border_color = config::to_hex((c.redComponent(), c.greenComponent(), c.blueComponent()));
            cfg.border_style = "solid".into();
            save(&cfg);
        }


        #[unsafe(method(done:))]
        fn done(&self, _sender: &AnyObject) {
            if let Some(w) = WINDOW.with(|w| w.borrow().clone()) {
                w.performClose(None);
            }
        }
    }
);

fn save(c: &Config) {
    if let Err(e) = config::save(c) {
        eprintln!("notch: settings save failed: {e}");
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}
fn login_plist() -> PathBuf {
    home().join("Library/LaunchAgents/local.notch.plist")
}
/// Where a switched-off login item is parked (outside LaunchAgents, so
/// launchd ignores it), to be put back as-is when switched on again.
fn parked_plist() -> PathBuf {
    home().join(".claude/notch/local.notch.plist.off")
}
/// The installed Notch.app's binary, if installed.
fn installed_binary() -> Option<PathBuf> {
    [PathBuf::from("/Applications"), home().join("Applications")]
        .into_iter()
        .map(|d| d.join("Notch.app/Contents/MacOS/notch"))
        .find(|p| p.exists())
}

fn set_login_item(on: bool) -> std::io::Result<()> {
    let (live, parked) = (login_plist(), parked_plist());
    if on {
        if parked.exists() {
            std::fs::rename(&parked, &live)
        } else {
            let bin = installed_binary().ok_or_else(|| std::io::Error::other("Notch.app is not installed"))?;
            std::fs::create_dir_all(live.parent().unwrap())?;
            std::fs::write(&live, plist_xml(&bin.to_string_lossy()))
        }
    } else if live.exists() {
        std::fs::create_dir_all(parked.parent().unwrap())?;
        std::fs::rename(&live, &parked)
    } else {
        Ok(())
    }
}

fn plist_xml(bin: &str) -> String {
    let bin = bin.replace('&', "&amp;").replace('<', "&lt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.notch</string>
  <key>ProgramArguments</key><array><string>{bin}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#
    )
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// One piece of the layout, positioned from the top of the window.
struct Placed {
    view: Retained<NSView>,
    x: f64,
    top: f64,
    w: f64,
    h: f64,
}

const W: f64 = 460.0;
const MARGIN: f64 = 24.0;
const ROW: f64 = 24.0;

struct Layout<'a> {
    target: &'a Target,
    mtm: MainThreadMarker,
    cfg: Config,
    items: Vec<Placed>,
    y: f64,
}

impl Layout<'_> {
    fn put(&mut self, view: Retained<NSView>, x: f64, top: f64, w: f64, h: f64) {
        self.items.push(Placed { view, x, top, w, h });
    }

    fn label(&self, text: &str, size: f64, weight: f64, color: &NSColor) -> Retained<NSTextField> {
        let l = NSTextField::labelWithString(&ns(text), self.mtm);
        l.setFont(Some(&NSFont::systemFontOfSize_weight(size, weight)));
        l.setTextColor(Some(color));
        l
    }

    /// Section: a small grey heading on top of a rounded box.
    fn section(&mut self, title: &str, rows: f64, fill: impl FnOnce(&mut Self, f64)) {
        self.y += 14.0;
        let head = self.label(title, 11.0, unsafe { NSFontWeightSemibold }, &NSColor::secondaryLabelColor());
        self.put(Retained::into_super(Retained::into_super(head)), MARGIN + 4.0, self.y, W - 2.0 * MARGIN, 16.0);
        self.y += 20.0;
        let box_h = rows * ROW + 16.0;
        let b = NSBox::new(self.mtm);
        b.setBoxType(NSBoxType::Custom);
        b.setCornerRadius(10.0);
        b.setBorderColor(&NSColor::separatorColor());
        b.setFillColor(&NSColor::controlBackgroundColor());
        b.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
        self.put(Retained::into_super(b), MARGIN, self.y, W - 2.0 * MARGIN, box_h);
        let top = self.y + 8.0;
        self.y += box_h;
        fill(self, top);
    }

    fn checkbox(&mut self, title: &str, key: &str, x: f64, top: f64, w: f64) {
        let tag = BOOL_KEYS.iter().position(|k| *k == key).expect("unknown setting") as isize;
        let on = self.cfg.get_bool(key);
        self.raw_checkbox(title, tag, on, true, x, top, w);
    }

    #[allow(clippy::too_many_arguments)]
    fn raw_checkbox(&mut self, title: &str, tag: isize, on: bool, enabled: bool, x: f64, top: f64, w: f64) {
        let b = unsafe {
            NSButton::checkboxWithTitle_target_action(&ns(title), Some(self.target), Some(sel!(toggle:)), self.mtm)
        };
        b.setTag(tag);
        b.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        b.setEnabled(enabled);
        self.put(Retained::into_super(Retained::into_super(b)), x, top, w, ROW);
    }
}

fn build_window(target: &Target, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let cfg = target.ivars().cfg.borrow().clone();
    let mut l = Layout { target, mtm, cfg, items: Vec::new(), y: 18.0 };
    let inner_x = MARGIN + 14.0;
    let inner_w = W - 2.0 * MARGIN - 28.0;
    let half = inner_w / 2.0;

    // header
    let title = l.label("Notch", 22.0, unsafe { NSFontWeightBold }, &NSColor::labelColor());
    l.put(Retained::into_super(Retained::into_super(title)), MARGIN, l.y, 200.0, 28.0);
    l.y += 28.0;
    let sub = l.label("การเปลี่ยนแปลงมีผลทันที", 12.0, 0.0, &NSColor::secondaryLabelColor());
    l.put(Retained::into_super(Retained::into_super(sub)), MARGIN, l.y, W - 2.0 * MARGIN, 18.0);
    l.y += 18.0;

    l.section("แท็บที่แสดงตอนขยาย", 4.0, |l, top| {
        let tabs = [
            ("♪  เพลง", "tab_music"),
            ("✦  Claude", "tab_claude"),
            ("ชั้นวางไฟล์", "tab_shelf"),
            ("คลิปบอร์ด", "tab_clipboard"),
            ("ตัวจับเวลา", "tab_timer"),
            ("อากาศ", "tab_weather"),
            ("กล้อง (กระจก)", "tab_camera"),
            ("ควบคุม", "tab_controls"),
        ];
        for (i, (t, k)) in tabs.iter().enumerate() {
            let (col, row) = (i / 4, i % 4);
            l.checkbox(t, k, inner_x + col as f64 * half, top + row as f64 * ROW, half);
        }
    });

    l.section("ตอน notch หดอยู่", 5.0, |l, top| {
        l.checkbox("เด้งออกด้านข้างตอนชี้เมาส์", "hover_bounce", inner_x, top, inner_w);
        l.checkbox("สถานะ Claude Code (ตัวหมุน ✻ ตอนกำลังทำงาน)", "claude_status", inner_x, top + ROW, inner_w);
        l.checkbox("ไอคอนกล้องตอนกล้องเปิดอยู่", "camera_dot", inner_x, top + 2.0 * ROW, inner_w);
        l.checkbox("แท่ง waveform เด้งตามเสียงจริง", "waveform", inner_x, top + 3.0 * ROW, inner_w);
        let lab = l.label("หุบเมื่อเพลงหยุดไปแล้ว", 13.0, 0.0, &NSColor::labelColor());
        l.put(Retained::into_super(Retained::into_super(lab)), inner_x + 2.0, top + 4.0 * ROW + 3.0, 180.0, 18.0);
        let pop =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(0.0, 0.0, 120.0, 24.0), false);
        for (_, t) in IDLE_CHOICES {
            pop.addItemWithTitle(&ns(t));
        }
        let cur = IDLE_CHOICES.iter().position(|(s, _)| *s == l.cfg.idle_secs).unwrap_or(1);
        pop.selectItemAtIndex(cur as isize);
        unsafe {
            pop.setTarget(Some(l.target));
            pop.setAction(Some(sel!(idle:)));
        }
        l.put(Retained::into_super(Retained::into_super(Retained::into_super(pop))), inner_x + inner_w - 130.0, top + 4.0 * ROW, 130.0, ROW);
    });

    l.section("แถบแจ้งเตือน", 3.0, |l, top| {
        l.checkbox("ตอนเสียบสายชาร์จ", "banner_charging", inner_x, top, inner_w);
        l.checkbox("แบตเหลือน้อย (20% และ 10%)", "banner_low_battery", inner_x, top + ROW, inner_w);
        l.checkbox("ปรับเสียง / ความสว่าง", "banner_levels", inner_x, top + 2.0 * ROW, inner_w);
    });

    l.section("มุมขวาบนตอนขยาย", 1.0, |l, top| {
        l.checkbox("CPU · RAM · อุณหภูมิ", "sysbar", inner_x, top, half);
        l.checkbox("แบตเตอรี่", "battery", inner_x + half, top, half);
    });

    l.section("หน้าตา", 3.0, |l, top| {
        l.checkbox("ขอบไฟ (ตอน notch ขยาย)", "rgb_border", inner_x, top, inner_w);
        let lab = l.label("บริเวณ", 13.0, 0.0, &NSColor::labelColor());
        l.put(Retained::into_super(Retained::into_super(lab)), inner_x + 2.0, top + 2.0 * ROW + 3.0, 80.0, 18.0);
        let scope = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(0.0, 0.0, 170.0, 24.0), false);
        for (_, t) in BORDER_SCOPES {
            scope.addItemWithTitle(&ns(t));
        }
        let cur = BORDER_SCOPES.iter().position(|(s, _)| *s == l.cfg.border_scope).unwrap_or(0);
        scope.selectItemAtIndex(cur as isize);
        unsafe {
            scope.setTarget(Some(l.target));
            scope.setAction(Some(sel!(borderScope:)));
        }
        l.put(Retained::into_super(Retained::into_super(Retained::into_super(scope))), inner_x + inner_w - 44.0 - 8.0 - 170.0, top + 2.0 * ROW, 170.0, ROW);
        let lab = l.label("สีขอบ", 13.0, 0.0, &NSColor::labelColor());
        l.put(Retained::into_super(Retained::into_super(lab)), inner_x + 2.0, top + ROW + 3.0, 80.0, 18.0);
        // colour well (solid colour)
        let well = NSColorWell::initWithFrame(NSColorWell::alloc(mtm), rect(0.0, 0.0, 44.0, 24.0));
        let (r, g, b) = config::parse_hex(&l.cfg.border_color).unwrap_or((1.0, 0.31, 0.85));
        well.setColor(&NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0));
        well.setTag(TAG_COLOR_WELL);
        well.setEnabled(l.cfg.border_style == "solid");
        unsafe {
            well.setTarget(Some(l.target));
            well.setAction(Some(sel!(borderColor:)));
        }
        l.put(Retained::into_super(Retained::into_super(well)), inner_x + inner_w - 44.0, top + ROW, 44.0, ROW);
        // style popup
        let pop = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(0.0, 0.0, 170.0, 24.0), false);
        for (_, t) in BORDER_STYLES {
            pop.addItemWithTitle(&ns(t));
        }
        let cur = BORDER_STYLES.iter().position(|(s, _)| *s == l.cfg.border_style).unwrap_or(0);
        pop.selectItemAtIndex(cur as isize);
        unsafe {
            pop.setTarget(Some(l.target));
            pop.setAction(Some(sel!(borderStyle:)));
        }
        l.put(Retained::into_super(Retained::into_super(Retained::into_super(pop))), inner_x + inner_w - 44.0 - 8.0 - 170.0, top + ROW, 170.0, ROW);
    });

    l.section("ทั่วไป", 2.0, |l, top| {
        l.checkbox("เปิดใช้ notch (ปิดเพื่อซ่อนชั่วคราว)", "notch_enabled", inner_x, top, inner_w);
        let top = top + ROW;
        let installed = installed_binary().is_some();
        let title = if installed { "เปิด Notch เองตอนเข้าเครื่อง" } else { "เปิด Notch เองตอนเข้าเครื่อง (ต้องติดตั้งก่อน)" };
        l.raw_checkbox(title, TAG_LOGIN, login_plist().exists(), installed, inner_x, top, inner_w);
    });

    // footer buttons
    l.y += 18.0;
    let button = |l: &Layout, title: &str, action| unsafe {
        NSButton::buttonWithTitle_target_action(&ns(title), Some(l.target), Some(action), mtm)
    };
    let reset = button(&l, "ค่าเริ่มต้น", sel!(reset:));
    let open = button(&l, "เสร็จ", sel!(done:));
    open.setKeyEquivalent(&ns("\r"));
    let y = l.y;
    l.put(Retained::into_super(Retained::into_super(reset)), MARGIN - 6.0, y, 110.0, 32.0);
    l.put(Retained::into_super(Retained::into_super(open)), W - MARGIN - 106.0 + 6.0, y, 106.0, 32.0);
    l.y += 32.0 + 16.0;

    let h = l.y;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, W, h),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&ns("ตั้งค่า Notch"));
    let content = window.contentView().expect("content view");
    // boxes first so checkboxes draw on top of them
    l.items.sort_by_key(|p| !p.view.isKindOfClass(NSBox::class()));
    for p in &l.items {
        p.view.setFrame(rect(p.x, h - p.top - p.h, p.w, p.h));
        content.addSubview(&p.view);
    }
    window.center();
    window
}

fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::new(mtm);
    let item = |title: &str, action, key: &str| unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), Some(action), &ns(key))
    };
    // both just close the settings window (the notch keeps running)
    app_menu.addItem(&item("ปิดหน้าต่าง", sel!(performClose:), "w"));
    app_menu.addItem(&item("ปิดการตั้งค่า", sel!(performClose:), "q"));
    app_item.setSubmenu(Some(&app_menu));
    bar.addItem(&app_item);
    bar
}

/// Set right before a real quit (from the notch's menu).
static REALLY_QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Quit Notch for real (a normal exit, so the login item's KeepAlive doesn't
/// restart it).
pub fn quit_app(mtm: MainThreadMarker) {
    REALLY_QUIT.store(true, std::sync::atomic::Ordering::SeqCst);
    NSApplication::sharedApplication(mtm).terminate(None);
}

thread_local! {
    static TARGET: RefCell<Option<Retained<Target>>> = const { RefCell::new(None) };
    static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
}

fn target(mtm: MainThreadMarker) -> Retained<Target> {
    TARGET.with(|t| {
        t.borrow_mut()
            .get_or_insert_with(|| {
                let t = Target::alloc(mtm).set_ivars(Ivars { cfg: RefCell::new(config::load()) });
                unsafe { msg_send![super(t), init] }
            })
            .clone()
    })
}

/// Call once at startup: the app delegate (so opening Notch.app again shows
/// the settings) and the menu used while the settings window is open.
pub fn install(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let t = target(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*t)));
    app.setMainMenu(Some(&main_menu(mtm)));
}

/// Open (or bring forward) the settings window.
pub fn show(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let t = target(mtm);
    let existing = WINDOW.with(|w| w.borrow().clone());
    let window = match existing {
        Some(w) => w,
        None => {
            // fresh values (the file may have been edited meanwhile)
            *t.ivars().cfg.borrow_mut() = config::load();
            let w = build_window(&t, mtm);
            w.setDelegate(Some(ProtocolObject::from_ref(&*t)));
            WINDOW.with(|slot| *slot.borrow_mut() = Some(w.clone()));
            w
        }
    };
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    window.makeKeyAndOrderFront(None);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

/// `notch --settings-snapshot FILE.png [dark]`: render the settings window's
/// contents to a PNG (for checking the layout without a screenshot).
pub fn snapshot(mtm: MainThreadMarker, path: &str, dark: bool) {
    use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBitmapImageFileType};
    let app = NSApplication::sharedApplication(mtm);
    let target: Retained<Target> = {
        let t = Target::alloc(mtm).set_ivars(Ivars { cfg: RefCell::new(Config::default()) });
        unsafe { msg_send![super(t), init] }
    };
    let name = unsafe { if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } };
    if let Some(ap) = NSAppearance::appearanceNamed(name) {
        app.setAppearance(Some(&ap));
    }
    let window = build_window(&target, mtm);
    let view = window.contentView().expect("content view");
    let b = view.bounds();
    let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(b) else { return };
    view.cacheDisplayInRect_toBitmapImageRep(b, &rep);
    let data = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &objc2_foundation::NSDictionary::new())
    };
    if let Some(d) = data {
        std::fs::write(path, d.to_vec()).unwrap();
        println!("wrote {path}");
    }
}
