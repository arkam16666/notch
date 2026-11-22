//! The settings window (GTK). Same options and the same file as the macOS
//! app (~/.claude/notch/config.json); every change is saved at once and the
//! island picks it up within a second.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::config::{self, Config};

thread_local! {
    static OPEN: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
}

fn autostart_override() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config/autostart/notch.desktop"))
}

/// Starts at login unless the user's autostart entry says Hidden=true.
fn autostart_on() -> bool {
    match autostart_override().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) => !s.lines().any(|l| l.trim() == "Hidden=true"),
        // no user entry: on if the package installed the system one
        None => std::path::Path::new("/etc/xdg/autostart/notch.desktop").exists(),
    }
}

fn set_autostart(on: bool) {
    let Some(p) = autostart_override() else { return };
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let exe = std::env::current_exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|_| "notch".into());
    let body = format!(
        "[Desktop Entry]\nType=Application\nName=Notch\nExec={exe}\nIcon=notch\nNoDisplay=true\nX-GNOME-Autostart-enabled={on}\n{}",
        if on { "" } else { "Hidden=true\n" }
    );
    let _ = std::fs::write(p, body);
}

pub fn show(app: &gtk::Application) {
    if let Some(w) = OPEN.with(|o| o.borrow().clone()) {
        w.present();
        return;
    }
    let cfg = Rc::new(RefCell::new(config::load()));
    let save = {
        let cfg = cfg.clone();
        move || {
            if let Err(e) = config::save(&cfg.borrow()) {
                eprintln!("notch: settings save failed: {e}");
            }
        }
    };

    let win = gtk::Window::builder().title("ตั้งค่า Notch").default_width(460).resizable(false).build();
    win.set_application(Some(app));
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    col.set_margin_top(20);
    col.set_margin_bottom(20);
    col.set_margin_start(24);
    col.set_margin_end(24);

    let title = gtk::Label::new(None);
    title.set_markup("<span size='x-large' weight='bold'>Notch</span>");
    title.set_halign(gtk::Align::Start);
    col.append(&title);
    let sub = gtk::Label::new(Some("การเปลี่ยนแปลงมีผลทันที"));
    sub.add_css_class("dim-label");
    sub.set_halign(gtk::Align::Start);
    col.append(&sub);

    let section = |name: &str| {
        let l = gtk::Label::new(None);
        l.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(name)));
        l.set_halign(gtk::Align::Start);
        l.set_margin_top(12);
        let frame = gtk::Frame::new(None);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 2);
        inner.set_margin_top(8);
        inner.set_margin_bottom(8);
        inner.set_margin_start(12);
        inner.set_margin_end(12);
        frame.set_child(Some(&inner));
        (l, frame, inner)
    };
    let check = |inner: &gtk::Box, label: &str, key: &'static str| {
        let b = gtk::CheckButton::with_label(label);
        b.set_active(cfg.borrow().get_bool(key));
        let (cfg, save) = (cfg.clone(), save.clone());
        b.connect_toggled(move |b| {
            cfg.borrow_mut().set_bool(key, b.is_active());
            save();
        });
        inner.append(&b);
    };

    let (l, f, inner) = section("แท็บที่แสดงตอนขยาย");
    col.append(&l);
    col.append(&f);
    let grid = gtk::Grid::builder().column_spacing(24).row_spacing(2).column_homogeneous(true).build();
    for (i, (label, key)) in [
        ("♪  เพลง", "tab_music"),
        ("✻  Claude", "tab_claude"),
        ("ตัวจับเวลา", "tab_timer"),
        ("คลิปบอร์ด", "tab_clipboard"),
        ("ควบคุม", "tab_controls"),
    ]
    .iter()
    .enumerate()
    {
        let b = gtk::CheckButton::with_label(label);
        b.set_active(cfg.borrow().get_bool(key));
        let (cfg, save, key) = (cfg.clone(), save.clone(), *key);
        b.connect_toggled(move |b| {
            cfg.borrow_mut().set_bool(key, b.is_active());
            save();
        });
        grid.attach(&b, (i / 3) as i32, (i % 3) as i32, 1, 1);
    }
    inner.append(&grid);

    let (l, f, inner) = section("ตอนเกาะหดอยู่");
    col.append(&l);
    col.append(&f);
    check(&inner, "เด้งออกด้านข้างตอนชี้เมาส์", "hover_bounce");
    check(&inner, "สถานะ Claude Code (ตัวหมุนตอนกำลังทำงาน)", "claude_status");

    let (l, f, inner) = section("แถบแจ้งเตือน");
    col.append(&l);
    col.append(&f);
    check(&inner, "ตอนเสียบสายชาร์จ", "banner_charging");
    check(&inner, "แบตเหลือน้อย (20% และ 10%)", "banner_low_battery");
    check(&inner, "ปรับเสียง / ความสว่าง", "banner_levels");
    check(&inner, "แบตเตอรี่มุมขวาบนตอนขยาย", "battery");

    let (l, f, inner) = section("หน้าตา");
    col.append(&l);
    col.append(&f);
    check(&inner, "ขอบไฟ (ตอนขยาย)", "rgb_border");
    let row = |label: &str| {
        let r = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let l = gtk::Label::new(Some(label));
        l.set_hexpand(true);
        l.set_halign(gtk::Align::Start);
        r.append(&l);
        r
    };
    let styles = [("rainbow", "รุ้ง RGB (หมุน)"), ("artwork", "ตามสีปกเพลง"), ("solid", "สีเดียว")];
    let r = row("สีขอบ");
    let style_dd = gtk::DropDown::from_strings(&styles.iter().map(|s| s.1).collect::<Vec<_>>());
    style_dd.set_selected(styles.iter().position(|s| s.0 == cfg.borrow().border_style).unwrap_or(0) as u32);
    let color = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    let (cr, cg, cb) = config::parse_hex(&cfg.borrow().border_color).unwrap_or((1.0, 0.31, 0.85));
    color.set_rgba(&gdk::RGBA::new(cr as f32, cg as f32, cb as f32, 1.0));
    color.set_sensitive(cfg.borrow().border_style == "solid");
    {
        let (cfg, save, color) = (cfg.clone(), save.clone(), color.clone());
        style_dd.connect_selected_notify(move |d| {
            let style = styles[d.selected() as usize % styles.len()].0;
            cfg.borrow_mut().border_style = style.into();
            color.set_sensitive(style == "solid");
            save();
        });
    }
    {
        let (cfg, save) = (cfg.clone(), save.clone());
        color.connect_rgba_notify(move |c| {
            let v = c.rgba();
            cfg.borrow_mut().border_color = config::to_hex((v.red() as f64, v.green() as f64, v.blue() as f64));
            save();
        });
    }
    r.append(&style_dd);
    r.append(&color);
    inner.append(&r);
    let scopes = [("notch", "รอบเกาะ"), ("screen", "ขอบจอทั้งจอ")];
    let r = row("บริเวณ");
    let scope_dd = gtk::DropDown::from_strings(&scopes.iter().map(|s| s.1).collect::<Vec<_>>());
    scope_dd.set_selected(scopes.iter().position(|s| s.0 == cfg.borrow().border_scope).unwrap_or(0) as u32);
    {
        let (cfg, save) = (cfg.clone(), save.clone());
        scope_dd.connect_selected_notify(move |d| {
            cfg.borrow_mut().border_scope = scopes[d.selected() as usize % scopes.len()].0.into();
            save();
        });
    }
    r.append(&scope_dd);
    inner.append(&r);

    let (l, f, inner) = section("ทั่วไป");
    col.append(&l);
    col.append(&f);
    let auto = gtk::CheckButton::with_label("เปิด Notch เองตอนเข้าเครื่อง");
    auto.set_active(autostart_on());
    auto.connect_toggled(|b| set_autostart(b.is_active()));
    inner.append(&auto);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_margin_top(16);
    let reset = gtk::Button::with_label("ค่าเริ่มต้น");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let done = gtk::Button::with_label("เสร็จ");
    done.add_css_class("suggested-action");
    {
        let (w, app) = (win.clone(), app.clone());
        reset.connect_clicked(move |_| {
            let _ = config::save(&Config::default());
            w.close();
            show(&app);
        });
    }
    {
        let w = win.clone();
        done.connect_clicked(move |_| w.close());
    }
    buttons.append(&reset);
    buttons.append(&spacer);
    buttons.append(&done);
    col.append(&buttons);

    win.set_child(Some(&col));
    win.connect_close_request(|_| {
        OPEN.with(|o| o.borrow_mut().take());
        glib::Propagation::Proceed
    });
    OPEN.with(|o| *o.borrow_mut() = Some(win.clone()));
    win.present();
}
