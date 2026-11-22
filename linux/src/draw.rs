//! Painting the island with cairo + pango, and hit-testing it. Pure: takes
//! a `Model`, so the same code draws the live window and `--snapshot` PNGs.
//!
//! The window is a fixed transparent strip at the top of the screen; the
//! island is drawn centred in it and only the island accepts input.

use std::f64::consts::PI;

use gtk::cairo::{self, Context, Filter, LinearGradient, SurfacePattern};
use gtk::pango::{self, EllipsizeMode, FontDescription};

use crate::model::{Banner, Detail, Hit, Model, Page, TIMER_PRESETS};
use crate::status::Activity;

pub const WIN_W: f64 = 720.0;
pub const WIN_H: f64 = 300.0;
const EAR: f64 = 8.0;
const HEADER: f64 = 36.0;
const PAD: f64 = 16.0;
const ORANGE: (f64, f64, f64) = (0.85, 0.47, 0.34); // Claude
const TIMER: (f64, f64, f64) = (1.0, 0.62, 0.2);
const GREEN: (f64, f64, f64) = (0.3, 0.85, 0.45);
const RED: (f64, f64, f64) = (1.0, 0.36, 0.32);
const BLUE: (f64, f64, f64) = (0.35, 0.6, 1.0);

pub fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn body_h(m: &Model) -> f64 {
    match m.page {
        Page::Music if m.out_open => 156f64.max(52.0 + m.sinks.len().min(6) as f64 * OUT_ROW),
        Page::Controls if m.detail.is_some() => 250.0,
        Page::Controls => 146.0,
        Page::Music => 156.0,
        Page::Claude => 150.0,
        Page::Timer => 140.0,
        Page::Clipboard => 34.0 + 7.0 * 26.0 + 10.0,
    }
}

/// Island rectangle: (x, width, height, corner radius).
pub fn island(m: &Model) -> (f64, f64, f64, f64) {
    let e = ease_out(m.t);
    let (cw, ch) = match m.banner_live() {
        Some(_) if m.t < 0.01 => (440.0, 34.0),
        _ if m.wings() => (380.0, 34.0),
        _ => (200.0, 30.0),
    };
    let grow = if m.cfg.hover_bounce { ease_out(m.hover_t) * (1.0 - e) } else { 0.0 };
    let (cw, ch) = (cw + 22.0 * grow, ch + 3.0 * grow);
    let (ew, eh) = (640.0, HEADER + body_h(m));
    let w = cw + (ew - cw) * e;
    let h = ch + (eh - ch) * e;
    ((WIN_W - w) / 2.0, w, h, 14.0 + 10.0 * e)
}

/// Clickable area (for the window's input region).
pub fn input_rect(m: &Model) -> (i32, i32, i32, i32) {
    let (x, w, h, _) = island(m);
    (x.floor() as i32, 0, w.ceil() as i32 + 1, h.ceil() as i32 + 1)
}

/// GNOME: the island hangs under the top bar instead of on the screen edge,
/// so it's a fully rounded pill (no flat top, no ears).
pub static FLOATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn floating() -> bool {
    FLOATING.load(std::sync::atomic::Ordering::Relaxed)
}

fn island_path(cr: &Context, x: f64, w: f64, h: f64, r: f64) {
    let r = r.min(h / 2.0).min(w / 2.0);
    if floating() {
        cr.new_path();
        round_rect(cr, x, 0.0, w, h, r);
        return;
    }
    cr.new_path();
    cr.move_to(x - EAR, 0.0);
    cr.curve_to(x - EAR * 0.4, 0.0, x, EAR * 0.6, x, EAR);
    cr.line_to(x, h - r);
    cr.arc_negative(x + r, h - r, r, PI, PI / 2.0);
    cr.line_to(x + w - r, h);
    cr.arc_negative(x + w - r, h - r, r, PI / 2.0, 0.0);
    cr.line_to(x + w, EAR);
    cr.curve_to(x + w, EAR * 0.6, x + w + EAR * 0.4, 0.0, x + w + EAR, 0.0);
    cr.close_path();
}

// ------------------------------------------------------------------ helpers

fn rgba(cr: &Context, (r, g, b): (f64, f64, f64), a: f64) {
    cr.set_source_rgba(r, g, b, a);
}

fn white(cr: &Context, a: f64) {
    cr.set_source_rgba(1.0, 1.0, 1.0, a);
}

fn round_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(h / 2.0).min(w / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

fn fill_round(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    round_rect(cr, x, y, w, h, r);
    let _ = cr.fill();
}

fn circle(cr: &Context, cx: f64, cy: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(cx, cy, r, 0.0, 2.0 * PI);
    let _ = cr.fill();
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// One line of text, top-left at (x, y), ellipsized to `w`.
#[allow(clippy::too_many_arguments)]
fn text(cr: &Context, s: &str, x: f64, y: f64, w: f64, size: f64, bold: bool, align: Align) {
    let layout = pangocairo::functions::create_layout(cr);
    let mut fd = FontDescription::from_string("Sans");
    fd.set_absolute_size(size * pango::SCALE as f64);
    fd.set_weight(if bold { pango::Weight::Bold } else { pango::Weight::Medium });
    layout.set_font_description(Some(&fd));
    layout.set_text(s);
    layout.set_width((w.max(1.0) * pango::SCALE as f64) as i32);
    layout.set_ellipsize(EllipsizeMode::End);
    layout.set_single_paragraph_mode(true);
    layout.set_alignment(match align {
        Align::Left => pango::Alignment::Left,
        Align::Center => pango::Alignment::Center,
        Align::Right => pango::Alignment::Right,
    });
    cr.move_to(x, y);
    pangocairo::functions::show_layout(cr, &layout);
}

fn mono(cr: &Context, s: &str, x: f64, y: f64, w: f64, size: f64, bold: bool, align: Align) {
    let layout = pangocairo::functions::create_layout(cr);
    let mut fd = FontDescription::from_string("Sans");
    fd.set_absolute_size(size * pango::SCALE as f64);
    fd.set_weight(if bold { pango::Weight::Bold } else { pango::Weight::Medium });
    layout.set_font_description(Some(&fd));
    // tabular digits so countdowns don't wobble
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrFontFeatures::new("tnum"));
    layout.set_attributes(Some(&attrs));
    layout.set_text(s);
    layout.set_width((w * pango::SCALE as f64) as i32);
    layout.set_alignment(match align {
        Align::Left => pango::Alignment::Left,
        Align::Center => pango::Alignment::Center,
        Align::Right => pango::Alignment::Right,
    });
    cr.move_to(x, y);
    pangocairo::functions::show_layout(cr, &layout);
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn art(cr: &Context, m: &Model, x: f64, y: f64, s: f64, r: f64, a: f64) {
    let _ = cr.save();
    round_rect(cr, x, y, s, s, r);
    cr.clip();
    match &m.art {
        Some(art) => {
            let k = s / art.surface.width() as f64;
            cr.translate(x, y);
            cr.scale(k, k);
            let _ = cr.set_source_surface(&art.surface, 0.0, 0.0);
            cr.source().set_filter(Filter::Good);
            let _ = cr.paint_with_alpha(a);
        }
        None => {
            white(cr, 0.1 * a);
            let _ = cr.paint();
            white(cr, 0.6 * a);
            note_icon(cr, x + s / 2.0, y + s / 2.0, s * 0.5);
        }
    }
    let _ = cr.restore();
}

// ----------------------------------------------------------------- glyphs

fn play_glyph(cr: &Context, cx: f64, cy: f64, s: f64) {
    cr.new_path();
    cr.move_to(cx - s * 0.35, cy - s * 0.5);
    cr.line_to(cx + s * 0.55, cy);
    cr.line_to(cx - s * 0.35, cy + s * 0.5);
    cr.close_path();
    let _ = cr.fill();
}

fn pause_glyph(cr: &Context, cx: f64, cy: f64, s: f64) {
    fill_round(cr, cx - s * 0.42, cy - s * 0.5, s * 0.3, s, s * 0.08);
    fill_round(cr, cx + s * 0.12, cy - s * 0.5, s * 0.3, s, s * 0.08);
}

fn skip_glyph(cr: &Context, cx: f64, cy: f64, s: f64, forward: bool) {
    let d = if forward { 1.0 } else { -1.0 };
    for off in [-0.45, 0.05] {
        let x0 = cx + d * s * off;
        cr.new_path();
        cr.move_to(x0, cy - s * 0.4);
        cr.line_to(x0 + d * s * 0.5, cy);
        cr.line_to(x0, cy + s * 0.4);
        cr.close_path();
        let _ = cr.fill();
    }
}

fn speaker_glyph(cr: &Context, x: f64, cy: f64, s: f64, waves: u8, muted: bool) {
    cr.new_path();
    cr.move_to(x, cy - s * 0.18);
    cr.line_to(x + s * 0.22, cy - s * 0.18);
    cr.line_to(x + s * 0.5, cy - s * 0.45);
    cr.line_to(x + s * 0.5, cy + s * 0.45);
    cr.line_to(x + s * 0.22, cy + s * 0.18);
    cr.line_to(x, cy + s * 0.18);
    cr.close_path();
    let _ = cr.fill();
    cr.set_line_width(s * 0.09);
    cr.set_line_cap(cairo::LineCap::Round);
    if muted {
        cr.move_to(x + s * 0.65, cy - s * 0.2);
        cr.line_to(x + s * 1.0, cy + s * 0.2);
        cr.move_to(x + s * 1.0, cy - s * 0.2);
        cr.line_to(x + s * 0.65, cy + s * 0.2);
        let _ = cr.stroke();
        return;
    }
    for i in 0..waves {
        let r = s * (0.32 + 0.2 * i as f64);
        cr.new_sub_path();
        cr.arc(x + s * 0.5, cy, r, -0.8, 0.8);
        let _ = cr.stroke();
    }
}

fn sun_glyph(cr: &Context, cx: f64, cy: f64, s: f64) {
    circle(cr, cx, cy, s * 0.22);
    cr.set_line_width(s * 0.09);
    cr.set_line_cap(cairo::LineCap::Round);
    for i in 0..8 {
        let a = i as f64 * PI / 4.0;
        cr.move_to(cx + a.cos() * s * 0.36, cy + a.sin() * s * 0.36);
        cr.line_to(cx + a.cos() * s * 0.5, cy + a.sin() * s * 0.5);
    }
    let _ = cr.stroke();
}

/// Claude's asterisk; `spin` turns it (radians) while it's busy.
fn asterisk(cr: &Context, cx: f64, cy: f64, r: f64, spin: f64) {
    cr.set_line_width(r * 0.32);
    cr.set_line_cap(cairo::LineCap::Round);
    for i in 0..6 {
        let a = spin + i as f64 * PI / 3.0;
        cr.move_to(cx + a.cos() * r * 0.15, cy + a.sin() * r * 0.15);
        cr.line_to(cx + a.cos() * r, cy + a.sin() * r);
    }
    let _ = cr.stroke();
}

fn note_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    let _ = cr.save();
    cr.translate(cx - s * 0.18, cy + s * 0.3);
    cr.scale(1.25, 1.0);
    circle(cr, 0.0, 0.0, s * 0.2);
    let _ = cr.restore();
    cr.rectangle(cx - s * 0.02, cy - s * 0.45, s * 0.1, s * 0.75);
    let _ = cr.fill();
    cr.move_to(cx + s * 0.08, cy - s * 0.45);
    cr.curve_to(cx + s * 0.3, cy - s * 0.35, cx + s * 0.42, cy - s * 0.2, cx + s * 0.3, cy);
    cr.curve_to(cx + s * 0.32, cy - s * 0.18, cx + s * 0.2, cy - s * 0.25, cx + s * 0.08, cy - s * 0.25);
    cr.close_path();
    let _ = cr.fill();
}

fn clock_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    cr.set_line_width(s * 0.12);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.new_sub_path();
    cr.arc(cx, cy + s * 0.05, s * 0.42, 0.0, 2.0 * PI);
    cr.move_to(cx, cy + s * 0.05);
    cr.line_to(cx, cy - s * 0.2);
    cr.move_to(cx, cy + s * 0.05);
    cr.line_to(cx + s * 0.17, cy + s * 0.15);
    cr.move_to(cx - s * 0.12, cy - s * 0.5);
    cr.line_to(cx + s * 0.12, cy - s * 0.5);
    let _ = cr.stroke();
}

fn copy_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    cr.set_line_width(s * 0.11);
    round_rect(cr, cx - s * 0.42, cy - s * 0.25, s * 0.6, s * 0.7, s * 0.1);
    let _ = cr.stroke();
    round_rect(cr, cx - s * 0.18, cy - s * 0.48, s * 0.6, s * 0.7, s * 0.1);
    let _ = cr.fill();
}

fn bolt_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    cr.new_path();
    cr.move_to(cx + s * 0.1, cy - s * 0.5);
    cr.line_to(cx - s * 0.3, cy + s * 0.08);
    cr.line_to(cx - s * 0.02, cy + s * 0.08);
    cr.line_to(cx - s * 0.12, cy + s * 0.5);
    cr.line_to(cx + s * 0.3, cy - s * 0.1);
    cr.line_to(cx + s * 0.02, cy - s * 0.1);
    cr.close_path();
    let _ = cr.fill();
}

fn spin_angle(now: f64) -> f64 {
    // a step every 120 ms, like Claude Code's spinner
    (now / 0.12).floor() * PI / 9.0
}

fn battery_glyph(cr: &Context, x: f64, cy: f64, pct: u8, charging: bool, a: f64) {
    let (w, h) = (24.0, 11.5);
    let y = cy - h / 2.0;
    cr.set_line_width(1.2);
    white(cr, 0.5 * a);
    round_rect(cr, x, y, w, h, 3.0);
    let _ = cr.stroke();
    fill_round(cr, x + w + 1.0, cy - 2.5, 2.0, 5.0, 1.0);
    let col = if charging { GREEN } else if pct <= 20 { RED } else { (1.0, 1.0, 1.0) };
    rgba(cr, col, a);
    fill_round(cr, x + 2.0, y + 2.0, ((w - 4.0) * pct as f64 / 100.0).max(2.0), h - 4.0, 1.5);
}

fn eq_bars(cr: &Context, x: f64, cy: f64, h: f64, color: (f64, f64, f64), a: f64, t: f64, playing: bool) {
    for i in 0..4 {
        let k = if playing { 0.35 + 0.65 * ((t * (5.0 + i as f64 * 1.7) + i as f64 * 1.3).sin() * 0.5 + 0.5) } else { 0.3 };
        let bh = h * k;
        rgba(cr, color, a);
        fill_round(cr, x + i as f64 * 5.0, cy - bh / 2.0, 3.0, bh, 1.5);
    }
}

// --------------------------------------------------------------- layouts

struct MusicL {
    art: (f64, f64, f64),
    tx: f64,
    tw: f64,
    top: f64,
    bar_y: f64,
    ctrl_cy: f64,
}

fn music_l(ix: f64, iw: f64) -> MusicL {
    let top = HEADER;
    let art_s = 118.0;
    let tx = ix + PAD + art_s + 18.0;
    MusicL { art: (ix + PAD, top + 12.0, art_s), tx, tw: ix + iw - PAD - tx, top, bar_y: top + 74.0, ctrl_cy: top + 122.0 }
}

fn tab_rect(ix: f64, i: usize) -> (f64, f64, f64, f64) {
    (ix + 14.0 + i as f64 * 34.0, 6.0, 30.0, 24.0)
}

fn timer_rects(ix: f64, iw: f64) -> ([(f64, f64, f64, f64); 5], (f64, f64, f64), (f64, f64, f64), (f64, f64, f64, f64)) {
    let (cw, gap) = (50.0, 6.0);
    let x0 = ix + iw - PAD - 5.0 * cw - 4.0 * gap;
    let top = HEADER;
    let presets = std::array::from_fn(|i| (x0 + i as f64 * (cw + gap), top + 18.0, cw, 30.0));
    (presets, (x0 + 23.0, top + 85.0, 23.0), (x0 + 79.0, top + 85.0, 19.0), (x0 + 104.0, top + 70.0, 80.0, 30.0))
}

fn clip_row(ix: f64, iw: f64, i: usize) -> (f64, f64, f64, f64) {
    (ix + PAD - 8.0, HEADER + 32.0 + i as f64 * 26.0, iw - 2.0 * PAD + 16.0, 24.0)
}

fn clear_rect(ix: f64, iw: f64) -> (f64, f64, f64, f64) {
    (ix + iw - PAD - 110.0, HEADER + 6.0, 110.0, 20.0)
}

fn inside((x, y, w, h): (f64, f64, f64, f64), px: f64, py: f64) -> bool {
    px >= x && px <= x + w && py >= y && py <= y + h
}

fn in_circle((cx, cy, r): (f64, f64, f64), px: f64, py: f64) -> bool {
    (px - cx).powi(2) + (py - cy).powi(2) <= r * r
}

// -------------------------------------------------------------- hit test

pub fn hit(m: &Model, px: f64, py: f64) -> Option<Hit> {
    let (ix, iw, ih, _) = island(m);
    if px < ix || px > ix + iw || py > ih {
        return None;
    }
    if !m.expanded || m.t < 0.9 {
        return Some(Hit::Island);
    }
    for (i, p) in m.pages().iter().enumerate() {
        if inside(tab_rect(ix, i), px, py) {
            return Some(Hit::Tab(*p));
        }
    }
    match m.page {
        Page::Music if m.out_open => {
            if inside(back_rect(ix), px, py) {
                return Some(Hit::OutBack);
            }
            if let Some(i) = (0..m.sinks.len().min(6)).find(|i| inside(out_row(ix, iw, *i), px, py)) {
                return Some(Hit::OutRow(i));
            }
        }
        Page::Music if m.track.is_some() => {
            let l = music_l(ix, iw);
            if in_circle((ix + iw - PAD - 14.0, l.ctrl_cy, 16.0), px, py) {
                return Some(Hit::OutBtn);
            }
            let cx = l.tx + l.tw / 2.0;
            if inside((l.tx, l.bar_y - 8.0, l.tw, 18.0), px, py) && m.track.as_ref().is_some_and(|t| t.can_seek && t.length > 0.0) {
                return Some(Hit::Seek(((px - l.tx) / l.tw).clamp(0.0, 1.0)));
            }
            for (h, x) in [(Hit::Prev, cx - 62.0), (Hit::Play, cx), (Hit::Next, cx + 62.0)] {
                if in_circle((x, l.ctrl_cy, 24.0), px, py) {
                    return Some(h);
                }
            }
        }
        Page::Timer => {
            let (presets, toggle, reset, plus) = timer_rects(ix, iw);
            if let Some(i) = presets.iter().position(|r| inside(*r, px, py)) {
                return Some(Hit::TimerPreset(i));
            }
            if in_circle(toggle, px, py) {
                return Some(Hit::TimerToggle);
            }
            if in_circle(reset, px, py) {
                return Some(Hit::TimerReset);
            }
            if inside(plus, px, py) {
                return Some(Hit::TimerPlus);
            }
        }
        Page::Controls => return Some(controls_hit(m, ix, iw, px, py)),
        Page::Clipboard => {
            if !m.clip.is_empty() && inside(clear_rect(ix, iw), px, py) {
                return Some(Hit::ClipClear);
            }
            if let Some(i) = (0..m.clip.len().min(7)).find(|i| inside(clip_row(ix, iw, *i), px, py)) {
                return Some(Hit::Clip(i));
            }
        }
        _ => {}
    }
    Some(Hit::Island)
}

// ------------------------------------------------------------------ paint

pub fn draw(cr: &Context, m: &Model) {
    cr.set_operator(cairo::Operator::Clear);
    let _ = cr.paint();
    cr.set_operator(cairo::Operator::Over);

    let (ix, iw, ih, r) = island(m);
    island_path(cr, ix, iw, ih, r);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    let _ = cr.fill_preserve();
    let _ = cr.save();
    cr.clip();

    let t = m.t;
    let now = m.started.elapsed().as_secs_f64();

    // blurred-artwork backdrop on the open music page
    if t > 0.05 && m.page == Page::Music && m.track.is_some() {
        if let Some(a) = &m.art {
            backdrop(cr, a, ix, iw, ih, t);
        }
    }

    // ---- collapsed ----
    let ca = (1.0 - t * 2.5).max(0.0);
    if ca > 0.0 {
        match m.banner_live() {
            Some(b) => draw_banner(cr, m, b, ix, iw, ih, ca),
            None if m.wings() => draw_wings(cr, m, ix, iw, ih, ca, now),
            None => {}
        }
    }

    // ---- expanded ----
    let ea = ((t - 0.45) / 0.55).clamp(0.0, 1.0).powi(2);
    if ea > 0.0 {
        draw_header(cr, m, ix, iw, ea, now);
        match m.page {
            Page::Music => draw_music(cr, m, ix, iw, ea),
            Page::Claude => draw_claude(cr, m, ix, iw, ea, now),
            Page::Timer => draw_timer(cr, m, ix, iw, ea),
            Page::Clipboard => draw_clipboard(cr, m, ix, iw, ea),
            Page::Controls => draw_controls(cr, m, ix, iw, ea),
        }
    }
    // glow border on the open island
    if t > 0.3 && m.cfg.rgb_border && !(m.cfg.border_scope == "screen" && m.edge_window) {
        border(cr, m, ix, iw, ih, r, ((t - 0.3) / 0.6).clamp(0.0, 1.0), now);
    }
    let _ = cr.restore();
}

/// HSV (0..1) → RGB
pub fn hsv(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Colour of the glow at `angle` (radians round the centre) at time `now`.
pub fn glow_color(m: &Model, angle: f64, now: f64) -> (f64, f64, f64) {
    glow_color_for(&m.cfg.border_style, &m.cfg.border_color, m.art.as_ref().map(|a| a.accent), angle, now)
}

pub fn glow_color_for(style: &str, hex: &str, accent: Option<(f64, f64, f64)>, angle: f64, now: f64) -> (f64, f64, f64) {
    let turn = angle / (2.0 * PI) + now / 5.0; // one lap per 5 s
    let lighten = |(r, g, b): (f64, f64, f64), k: f64| (r + (1.0 - r) * k, g + (1.0 - g) * k, b + (1.0 - b) * k);
    let glint = (0.5 + 0.5 * (turn * 2.0 * PI).cos()).powi(6);
    match style {
        "artwork" => lighten(accent.unwrap_or((0.8, 0.5, 1.0)), 0.15 + 0.55 * glint),
        "solid" => lighten(crate::config::parse_hex(hex).unwrap_or((1.0, 0.31, 0.85)), 0.6 * glint),
        _ => hsv(turn, 0.85, 1.0),
    }
}

/// Glow along all four screen edges: a crisp line plus two soft bands.
pub fn edge_ring(cr: &Context, w: f64, h: f64, a: f64, now: f64, color: impl Fn(f64, f64) -> (f64, f64, f64)) {
    cr.set_operator(cairo::Operator::Clear);
    let _ = cr.paint();
    cr.set_operator(cairo::Operator::Over);
    if a <= 0.0 {
        return;
    }
    let (cx, cy) = (w / 2.0, h / 2.0);
    for (width, alpha) in [(24.0, 0.1), (10.0, 0.28), (3.5, 1.0)] {
        let inset = width / 2.0;
        round_rect(cr, inset, inset, w - 2.0 * inset, h - 2.0 * inset, (10.0 - inset).max(2.0));
        let Ok(path) = cr.copy_path_flat() else { continue };
        cr.new_path();
        let mut pts: Vec<(f64, f64)> = Vec::new();
        for seg in path.iter() {
            match seg {
                cairo::PathSegment::MoveTo((x, y)) | cairo::PathSegment::LineTo((x, y)) => pts.push((x, y)),
                cairo::PathSegment::ClosePath => {
                    if let Some(f) = pts.first().copied() {
                        pts.push(f);
                    }
                }
                _ => {}
            }
        }
        cr.set_line_width(width);
        cr.set_line_cap(cairo::LineCap::Butt);
        for p in pts.windows(2) {
            let ((x0, y0), (x1, y1)) = (p[0], p[1]);
            let n = ((x1 - x0).hypot(y1 - y0) / 24.0).ceil().max(1.0) as usize;
            for k in 0..n {
                let (f0, f1) = (k as f64 / n as f64, (k + 1) as f64 / n as f64);
                let (ax, ay) = (x0 + (x1 - x0) * f0, y0 + (y1 - y0) * f0);
                let (bx, by) = (x0 + (x1 - x0) * f1, y0 + (y1 - y0) * f1);
                let angle = ((ay + by) / 2.0 - cy).atan2((ax + bx) / 2.0 - cx);
                let c = color(angle, now);
                cr.set_source_rgba(c.0, c.1, c.2, alpha * a);
                cr.move_to(ax, ay);
                cr.line_to(bx, by);
                let _ = cr.stroke();
            }
        }
    }
}

/// The glow ring: the island outline (not its top edge) stroked in short
/// segments, each coloured by its angle round the centre.
#[allow(clippy::too_many_arguments)]
fn border(cr: &Context, m: &Model, ix: f64, iw: f64, ih: f64, r: f64, a: f64, now: f64) {
    island_path(cr, ix, iw, ih, r);
    let Ok(path) = cr.copy_path_flat() else { return };
    cr.new_path();
    let (cx, cy) = (ix + iw / 2.0, ih / 2.0);
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for seg in path.iter() {
        match seg {
            cairo::PathSegment::MoveTo((x, y)) | cairo::PathSegment::LineTo((x, y)) => pts.push((x, y)),
            cairo::PathSegment::ClosePath => {
                if let Some(f) = pts.first().copied() {
                    pts.push(f);
                }
            }
            _ => {}
        }
    }
    // subdivide long straight edges so the colour flows smoothly
    let mut fine: Vec<(f64, f64)> = Vec::new();
    for w in pts.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        let n = ((x1 - x0).hypot(y1 - y0) / 6.0).ceil().max(1.0) as usize;
        for k in 0..n {
            let f = k as f64 / n as f64;
            fine.push((x0 + (x1 - x0) * f, y0 + (y1 - y0) * f));
        }
    }
    cr.set_line_width(5.0);
    cr.set_line_cap(cairo::LineCap::Round);
    for w in fine.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if y0 < 1.5 && y1 < 1.5 && !floating() {
            continue; // the top edge is the screen edge
        }
        let angle = ((y0 + y1) / 2.0 - cy).atan2((x0 + x1) / 2.0 - cx);
        rgba(cr, glow_color(m, angle, now), a);
        cr.move_to(x0, y0);
        cr.line_to(x1, y1);
        let _ = cr.stroke();
    }
}

fn backdrop(cr: &Context, a: &crate::model::Art, ix: f64, iw: f64, ih: f64, t: f64) {
    // sample the artwork down to 6×6 and stretch it back up: a soft colour field
    let Ok(small) = cairo::ImageSurface::create(cairo::Format::ARgb32, 6, 6) else { return };
    if let Ok(c) = Context::new(&small) {
        c.scale(6.0 / a.surface.width() as f64, 6.0 / a.surface.height() as f64);
        let _ = c.set_source_surface(&a.surface, 0.0, 0.0);
        c.source().set_filter(Filter::Good);
        let _ = c.paint();
    }
    let _ = cr.save();
    cr.translate(ix - 30.0, -30.0);
    cr.scale((iw + 60.0) / 6.0, (ih + 60.0) / 6.0);
    let pat = SurfacePattern::create(&small);
    pat.set_filter(Filter::Bilinear);
    pat.set_extend(cairo::Extend::Pad);
    let _ = cr.set_source(&pat);
    let _ = cr.paint_with_alpha(0.8 * t);
    let _ = cr.restore();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.5 * t);
    let _ = cr.paint();
    let g = LinearGradient::new(0.0, 0.0, 0.0, ih);
    g.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
    g.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 0.5 * t);
    let _ = cr.set_source(&g);
    let _ = cr.paint();
}

fn draw_wings(cr: &Context, m: &Model, ix: f64, iw: f64, ih: f64, a: f64, now: f64) {
    let cy = ih / 2.0;
    let busy = m.sessions.first().filter(|_| m.claude_busy());
    // left: artwork, or the Claude spinner / timer icon
    if m.track.is_some() {
        art(cr, m, ix + 12.0, cy - 11.0, 22.0, 6.0, a);
    } else if !m.sessions.is_empty() {
        rgba(cr, ORANGE, a);
        asterisk(cr, ix + 23.0, cy, 8.0, if busy.is_some() { spin_angle(now) } else { 0.0 });
    } else if m.timer.is_some() {
        rgba(cr, TIMER, a);
        clock_icon(cr, ix + 23.0, cy, 16.0);
    }
    // middle: song title (there's no camera notch in the way on Linux)
    if let Some(tr) = &m.track {
        white(cr, 0.85 * a);
        let label = if tr.artist.is_empty() { tr.title.clone() } else { format!("{} · {}", tr.title, tr.artist) };
        text(cr, &label, ix + 44.0, cy - 8.0, iw - 44.0 - 78.0, 12.5, false, Align::Left);
    } else if let Some(s) = m.sessions.first() {
        let label = match &s.activity {
            Activity::Thinking => "Claude กำลังคิด…".to_string(),
            Activity::Tool(t) => format!("Claude · {t}"),
            Activity::Waiting => "Claude รอคุณอยู่".into(),
            Activity::Done => "Claude เสร็จแล้ว ✓".into(),
        };
        rgba(cr, ORANGE, 0.95 * a);
        text(cr, &label, ix + 44.0, cy - 8.0, iw - 44.0 - 78.0, 12.5, false, Align::Left);
    }
    // right: countdown, Claude spinner or equaliser
    let rx = ix + iw - 66.0;
    if let Some(tm) = m.timer {
        rgba(cr, if tm.running() { TIMER } else { (0.6, 0.6, 0.6) }, a);
        mono(cr, &fmt_time(tm.remaining().ceil()), rx, cy - 8.5, 56.0, 13.0, true, Align::Right);
    } else if busy.is_some() && m.track.is_some() {
        rgba(cr, ORANGE, a);
        asterisk(cr, ix + iw - 24.0, cy, 8.0, spin_angle(now));
    } else if let Some(tr) = &m.track {
        let col = m.art.as_ref().map(|a| a.accent).unwrap_or((0.8, 0.5, 1.0));
        eq_bars(cr, ix + iw - 34.0, cy, 13.0, col, a, now, tr.playing);
    }
}

fn draw_banner(cr: &Context, _m: &Model, b: Banner, ix: f64, iw: f64, ih: f64, a: f64) {
    let cy = ih / 2.0;
    let (label, col): (String, (f64, f64, f64)) = match b {
        Banner::Volume(v, muted, d) => (
            if muted || v == 0.0 { "ปิดเสียง" } else if d > 0 { "เพิ่มเสียง" } else if d < 0 { "เบาเสียง" } else { "ระดับเสียง" }.into(),
            if muted { RED } else { (1.0, 1.0, 1.0) },
        ),
        Banner::Brightness(_, d) => (if d > 0 { "เพิ่มความสว่าง" } else { "ลดความสว่าง" }.into(), (1.0, 0.82, 0.3)),
        Banner::Charging(_) => ("กำลังชาร์จ".into(), GREEN),
        Banner::LowBattery(_) => ("แบตเหลือน้อย".into(), RED),
        Banner::TimerDone(_) => ("หมดเวลา".into(), TIMER),
    };
    // icon
    rgba(cr, col, a);
    match b {
        Banner::Volume(v, muted, _) => speaker_glyph(cr, ix + 14.0, cy, 16.0, if v < 0.34 { 1 } else if v < 0.67 { 2 } else { 3 }, muted || v == 0.0),
        Banner::Brightness(..) => sun_glyph(cr, ix + 24.0, cy, 18.0),
        Banner::Charging(_) => bolt_icon(cr, ix + 23.0, cy, 18.0),
        Banner::LowBattery(_) => text(cr, "!", ix + 12.0, cy - 10.0, 22.0, 16.0, true, Align::Center),
        Banner::TimerDone(_) => clock_icon(cr, ix + 23.0, cy, 17.0),
    }
    white(cr, a);
    text(cr, &label, ix + 42.0, cy - 8.5, 160.0, 13.0, true, Align::Left);
    // right side
    let right = ix + iw - 14.0;
    match b {
        Banner::Volume(..) | Banner::Brightness(..) => {
            let v = match b {
                Banner::Volume(_, true, _) => 0.0,
                Banner::Volume(v, ..) | Banner::Brightness(v, _) => v,
                _ => 0.0,
            };
            let bw = 120.0;
            let bx = right - 44.0 - bw;
            white(cr, 0.2 * a);
            fill_round(cr, bx, cy - 3.0, bw, 6.0, 3.0);
            if v > 0.0 {
                rgba(cr, col, a);
                fill_round(cr, bx, cy - 3.0, (bw * v).max(6.0), 6.0, 3.0);
            }
            white(cr, 0.9 * a);
            mono(cr, &format!("{}%", (v * 100.0).round() as u32), right - 40.0, cy - 8.0, 40.0, 12.5, true, Align::Right);
        }
        Banner::Charging(p) | Banner::LowBattery(p) => {
            rgba(cr, col, a);
            mono(cr, &format!("{p}%"), right - 80.0, cy - 8.5, 44.0, 13.0, true, Align::Right);
            battery_glyph(cr, right - 30.0, cy, p, matches!(b, Banner::Charging(_)), a);
        }
        Banner::TimerDone(min) => {
            rgba(cr, TIMER, a);
            text(cr, &format!("{min} นาที"), right - 120.0, cy - 8.5, 120.0, 13.0, true, Align::Right);
        }
    }
}

fn draw_header(cr: &Context, m: &Model, ix: f64, iw: f64, a: f64, now: f64) {
    for (i, p) in m.pages().iter().enumerate() {
        let (x, y, w, h) = tab_rect(ix, i);
        let selected = m.page == *p;
        let hovered = m.hover_hit == Some(Hit::Tab(*p));
        if selected || hovered {
            white(cr, if selected { 0.18 } else { 0.09 } * a);
            fill_round(cr, x, y, w, h, h / 2.0);
        }
        let col = match p {
            Page::Claude => ORANGE,
            Page::Timer if m.timer.is_some() => TIMER,
            _ => (1.0, 1.0, 1.0),
        };
        rgba(cr, col, if selected || *p == Page::Claude { 1.0 } else { 0.55 } * a);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        match p {
            Page::Music => note_icon(cr, cx, cy, 15.0),
            Page::Claude => asterisk(cr, cx, cy, 6.5, if m.claude_busy() { spin_angle(now) } else { 0.0 }),
            Page::Timer => clock_icon(cr, cx, cy, 14.0),
            Page::Clipboard => copy_icon(cr, cx, cy, 14.0),
            Page::Controls => switch_icon(cr, cx, cy, 14.0),
        }
    }
    if let Some((pct, charging)) = m.battery.filter(|_| m.cfg.battery) {
        let right = ix + iw - PAD;
        battery_glyph(cr, right - 28.0, 18.0, pct, charging, a);
        white(cr, 0.9 * a);
        mono(cr, &format!("{pct}%"), right - 80.0, 10.0, 46.0, 12.5, true, Align::Right);
    }
}

fn draw_music(cr: &Context, m: &Model, ix: f64, iw: f64, a: f64) {
    if m.out_open {
        return draw_outputs(cr, m, ix, iw, a);
    }
    let Some(tr) = &m.track else {
        white(cr, 0.35 * a);
        note_icon(cr, ix + iw / 2.0, HEADER + 46.0, 30.0);
        white(cr, 0.75 * a);
        text(cr, "ยังไม่มีเพลงเล่นอยู่", ix, HEADER + 72.0, iw, 14.0, true, Align::Center);
        white(cr, 0.4 * a);
        text(cr, "เล่นเพลงจาก Spotify, เบราว์เซอร์ หรือแอปเพลงไหนก็ได้", ix, HEADER + 96.0, iw, 12.0, false, Align::Center);
        return;
    };
    let l = music_l(ix, iw);
    art(cr, m, l.art.0, l.art.1, l.art.2, 16.0, a);
    white(cr, a);
    text(cr, &tr.title, l.tx, l.top + 14.0, l.tw - 30.0, 16.0, true, Align::Left);
    white(cr, 0.6 * a);
    text(cr, &tr.artist, l.tx, l.top + 38.0, l.tw - 30.0, 13.0, false, Align::Left);
    let accent = m.art.as_ref().map(|a| a.accent).unwrap_or((0.8, 0.5, 1.0));
    eq_bars(cr, ix + iw - PAD - 18.0, l.top + 24.0, 13.0, accent, a, m.started.elapsed().as_secs_f64(), tr.playing);

    // progress
    if tr.length > 0.0 {
        let pos = m.position();
        let frac = (pos / tr.length).clamp(0.0, 1.0);
        white(cr, 0.18 * a);
        fill_round(cr, l.tx, l.bar_y, l.tw, 4.0, 2.0);
        rgba(cr, accent, a);
        fill_round(cr, l.tx, l.bar_y, (l.tw * frac).max(4.0), 4.0, 2.0);
        white(cr, 0.55 * a);
        mono(cr, &fmt_time(pos), l.tx, l.bar_y + 9.0, 80.0, 11.0, false, Align::Left);
        mono(cr, &format!("-{}", fmt_time(tr.length - pos)), l.tx + l.tw - 80.0, l.bar_y + 9.0, 80.0, 11.0, false, Align::Right);
    } else {
        white(cr, 0.4 * a);
        text(cr, &tr.app, l.tx, l.bar_y - 4.0, l.tw, 11.5, false, Align::Left);
    }

    // controls
    let cx = l.tx + l.tw / 2.0;
    let cy = l.ctrl_cy;
    for (h, x) in [(Hit::Prev, cx - 62.0), (Hit::Next, cx + 62.0)] {
        if m.hover_hit == Some(h) {
            white(cr, 0.12 * a);
            circle(cr, x, cy, 20.0);
        }
        white(cr, a);
        skip_glyph(cr, x, cy, 18.0, h == Hit::Next);
    }
    // play button: white disc with a tight glow
    if tr.playing || m.hover_hit == Some(Hit::Play) {
        let g = gtk::cairo::RadialGradient::new(cx, cy, 19.0, cx, cy, 27.0);
        g.add_color_stop_rgba(0.0, accent.0, accent.1, accent.2, 0.35 * a);
        g.add_color_stop_rgba(1.0, accent.0, accent.1, accent.2, 0.0);
        let _ = cr.set_source(&g);
        circle(cr, cx, cy, 27.0);
    }
    // choose-speaker button (blue when not on the built-in output)
    let away = m.sinks.iter().find(|s| s.current).is_some_and(|s| s.bluetooth || s.headphones);
    let (ox, oy) = (ix + iw - PAD - 14.0, cy);
    if m.hover_hit == Some(Hit::OutBtn) {
        white(cr, 0.12 * a);
        circle(cr, ox, oy, 15.0);
    }
    if away {
        rgba(cr, BLUE, a);
    } else {
        white(cr, 0.7 * a);
    }
    speaker_out_icon(cr, ox, oy, 15.0);

    white(cr, 0.95 * a);
    circle(cr, cx, cy, 21.0);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.9 * a);
    if tr.playing {
        pause_glyph(cr, cx, cy, 16.0);
    } else {
        play_glyph(cr, cx + 1.5, cy, 16.0);
    }
}

fn limit_row(cr: &Context, label: &str, pct: Option<f64>, reset: &str, ix: f64, iw: f64, y: f64, a: f64) {
    white(cr, 0.85 * a);
    text(cr, label, ix + PAD, y, 120.0, 13.0, true, Align::Left);
    let bx = ix + PAD + 120.0;
    let bw = iw - 2.0 * PAD - 120.0 - 60.0;
    white(cr, 0.14 * a);
    fill_round(cr, bx, y + 6.0, bw, 6.0, 3.0);
    if let Some(p) = pct {
        let col = if p >= 90.0 { RED } else if p >= 70.0 { TIMER } else { ORANGE };
        rgba(cr, col, a);
        fill_round(cr, bx, y + 6.0, (bw * p / 100.0).max(6.0), 6.0, 3.0);
        white(cr, 0.9 * a);
        mono(cr, &format!("{}%", p.round() as i64), ix + iw - PAD - 56.0, y, 56.0, 13.0, true, Align::Right);
    }
    white(cr, 0.45 * a);
    text(cr, reset, bx, y + 16.0, bw, 11.0, false, Align::Left);
}

fn draw_claude(cr: &Context, m: &Model, ix: f64, iw: f64, a: f64, now: f64) {
    let top = HEADER;
    let reset_txt = |l: &Option<crate::claude_api::Limit>| {
        l.as_ref()
            .and_then(|l| l.resets_at)
            .map(|r| format!("รีเซ็ต {}", r.format("%a %H:%M")))
            .unwrap_or_default()
    };
    match &m.limits {
        None => {
            white(cr, 0.45 * a);
            text(cr, "กำลังโหลด % การใช้งาน…", ix, top + 40.0, iw, 13.0, false, Align::Center);
        }
        Some(Err(e)) => {
            white(cr, 0.5 * a);
            text(cr, e, ix + PAD, top + 40.0, iw - 2.0 * PAD, 12.5, false, Align::Center);
        }
        Some(Ok(l)) => {
            limit_row(cr, "5 ชั่วโมง", l.five_hour.as_ref().map(|x| x.pct), &reset_txt(&l.five_hour), ix, iw, top + 14.0, a);
            limit_row(cr, "รายสัปดาห์", l.week.as_ref().map(|x| x.pct), &reset_txt(&l.week), ix, iw, top + 56.0, a);
        }
    }
    // sessions
    let y = top + 104.0;
    white(cr, 0.08 * a);
    cr.rectangle(ix + PAD, y - 8.0, iw - 2.0 * PAD, 1.0);
    let _ = cr.fill();
    match m.sessions.first() {
        None => {
            white(cr, 0.4 * a);
            text(cr, "Claude Code ไม่ได้ทำงานอยู่", ix + PAD, y + 4.0, iw - 2.0 * PAD, 12.5, false, Align::Left);
        }
        Some(s) => {
            let (busy, label) = match &s.activity {
                Activity::Thinking => (true, "กำลังคิด…".to_string()),
                Activity::Tool(t) => (true, format!("ใช้ {t}")),
                Activity::Waiting => (false, "รอคุณตอบอยู่".into()),
                Activity::Done => (false, "เสร็จแล้ว ✓".into()),
            };
            rgba(cr, ORANGE, a);
            asterisk(cr, ix + PAD + 9.0, y + 11.0, 8.0, if busy { spin_angle(now) } else { 0.0 });
            white(cr, 0.9 * a);
            text(cr, &format!("{} · {label}", s.project), ix + PAD + 26.0, y + 3.0, iw - 2.0 * PAD - 120.0, 13.0, true, Align::Left);
            if m.sessions.len() > 1 {
                white(cr, 0.45 * a);
                text(cr, &format!("+{} เซสชัน", m.sessions.len() - 1), ix + iw - PAD - 100.0, y + 4.0, 100.0, 12.0, false, Align::Right);
            }
        }
    }
}

fn draw_timer(cr: &Context, m: &Model, ix: f64, iw: f64, a: f64) {
    let top = HEADER;
    let (presets, toggle, reset, plus) = timer_rects(ix, iw);
    let done = m.timer.is_none() && m.timer_done.is_some_and(|t| t.elapsed().as_secs_f64() < 5.0);
    let (rem, total, running) = match m.timer {
        Some(t) => (t.remaining().ceil(), t.total, t.running()),
        None => (if done { 0.0 } else { m.timer_last as f64 * 60.0 }, m.timer_last as f64 * 60.0, false),
    };
    let left_w = presets[0].0 - ix - PAD - 28.0;
    let digits_a = if done { 1.0 } else if m.timer.is_none() { 0.35 } else if running { 1.0 } else { 0.6 };
    if done {
        rgba(cr, TIMER, a);
    } else {
        white(cr, digits_a * a);
    }
    mono(cr, &fmt_time(rem), ix + PAD, top + 6.0, left_w, 50.0, true, Align::Left);
    let by = top + 74.0;
    white(cr, 0.12 * a);
    fill_round(cr, ix + PAD + 2.0, by, left_w - 4.0, 5.0, 2.5);
    let frac = if m.timer.is_some() && total > 0.0 { 1.0 - rem / total } else if done { 1.0 } else { 0.0 };
    if frac > 0.0 {
        rgba(cr, TIMER, a);
        fill_round(cr, ix + PAD + 2.0, by, ((left_w - 4.0) * frac).max(5.0), 5.0, 2.5);
    }
    let status = if done {
        "หมดเวลาแล้ว!".to_string()
    } else if let Some(t) = m.timer {
        if running {
            let end = chrono::Local::now() + chrono::Duration::seconds(t.remaining().ceil() as i64);
            format!("กำลังจับเวลา · เสร็จตอน {}", end.format("%H:%M"))
        } else {
            "หยุดชั่วคราว".into()
        }
    } else {
        "เลือกเวลา หรือกด ▶ เพื่อเริ่ม".into()
    };
    if done {
        rgba(cr, TIMER, a);
    } else {
        white(cr, 0.5 * a);
    }
    text(cr, &status, ix + PAD + 2.0, by + 12.0, left_w, 12.0, false, Align::Left);

    for (i, (x, y, w, h)) in presets.iter().enumerate() {
        let mins = TIMER_PRESETS[i];
        let selected = m.timer.is_some_and(|t| (t.total - mins as f64 * 60.0).abs() < 0.5);
        let hovered = m.hover_hit == Some(Hit::TimerPreset(i));
        if selected {
            rgba(cr, TIMER, 0.9 * a);
        } else {
            white(cr, if hovered { 0.16 } else { 0.09 } * a);
        }
        fill_round(cr, *x, *y, *w, *h, h / 2.0);
        if selected {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.85 * a);
        } else {
            white(cr, 0.85 * a);
        }
        text(cr, &format!("{mins} น."), *x, y + 7.0, *w, 12.5, true, Align::Center);
    }
    let (tx, ty, tr) = toggle;
    white(cr, if m.hover_hit == Some(Hit::TimerToggle) { 1.0 } else { 0.92 } * a);
    circle(cr, tx, ty, tr);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.9 * a);
    if running {
        pause_glyph(cr, tx, ty, 15.0);
    } else {
        play_glyph(cr, tx + 1.5, ty, 15.0);
    }
    let (rx, ry, rr) = reset;
    white(cr, if m.hover_hit == Some(Hit::TimerReset) && m.timer.is_some() { 0.2 } else { 0.1 } * a);
    circle(cr, rx, ry, rr);
    white(cr, if m.timer.is_some() { 0.9 } else { 0.35 } * a);
    text(cr, "↺", rx - rr, ry - 10.0, 2.0 * rr, 16.0, true, Align::Center);
    let (px, py, pw, ph) = plus;
    white(cr, if m.hover_hit == Some(Hit::TimerPlus) { 0.16 } else { 0.09 } * a);
    fill_round(cr, px, py, pw, ph, ph / 2.0);
    white(cr, 0.85 * a);
    text(cr, "+1 นาที", px, py + 7.0, pw, 12.5, true, Align::Center);
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect()
}

fn draw_clipboard(cr: &Context, m: &Model, ix: f64, iw: f64, a: f64) {
    let top = HEADER;
    let n = m.clip.len();
    white(cr, 0.5 * a);
    text(
        cr,
        &if n == 0 { "คลิปบอร์ด".to_string() } else { format!("คลิปบอร์ด · {n} รายการล่าสุด") },
        ix + PAD,
        top + 8.0,
        260.0,
        12.0,
        true,
        Align::Left,
    );
    if n == 0 {
        white(cr, 0.3 * a);
        copy_icon(cr, ix + iw / 2.0, top + 72.0, 28.0);
        white(cr, 0.55 * a);
        text(cr, "คัดลอกข้อความที่ไหนก็ได้ แล้วจะมาอยู่ที่นี่", ix, top + 100.0, iw, 13.0, false, Align::Center);
        white(cr, 0.35 * a);
        text(cr, "เก็บไว้ในหน่วยความจำเท่านั้น", ix, top + 122.0, iw, 11.5, false, Align::Center);
        return;
    }
    let (cx, cy, cw, _) = clear_rect(ix, iw);
    white(cr, 0.5 * a);
    text(cr, "ล้างทั้งหมด", cx, cy + 2.0, cw, 11.5, true, Align::Right);
    let copied = m.clip_copied.is_some_and(|t| t.elapsed().as_secs_f64() < 1.5);
    for i in 0..n.min(7) {
        let (x, y, w, h) = clip_row(ix, iw, i);
        let (s, at) = &m.clip[i];
        let hovered = m.hover_hit == Some(Hit::Clip(i));
        if hovered {
            white(cr, 0.08 * a);
            fill_round(cr, x, y, w, h, 8.0);
        }
        white(cr, 0.88 * a);
        text(cr, &one_line(s), x + 10.0, y + 4.0, w - 140.0, 12.5, false, Align::Left);
        let right = (x + w - 128.0, y + 5.0, 118.0);
        if copied && i == 0 {
            rgba(cr, GREEN, a);
            text(cr, "✓ คัดลอกแล้ว", right.0, right.1, right.2, 11.5, true, Align::Right);
        } else if hovered {
            white(cr, 0.6 * a);
            text(cr, "คลิกเพื่อคัดลอก", right.0, right.1, right.2, 11.5, true, Align::Right);
        } else {
            let secs = at.elapsed().as_secs();
            let ago = match secs {
                0..=59 => "เมื่อกี้".to_string(),
                60..=3599 => format!("{} นาที", secs / 60),
                _ => format!("{} ชม.", secs / 3600),
            };
            white(cr, 0.35 * a);
            text(cr, &ago, right.0, right.1, right.2, 11.0, false, Align::Right);
        }
    }
    let _ = BLUE;
}

// ----------------------------------------------------------- more glyphs

fn switch_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    cr.set_line_width(s * 0.11);
    round_rect(cr, cx - s * 0.45, cy - s * 0.42, s * 0.9, s * 0.36, s * 0.18);
    let _ = cr.stroke();
    circle(cr, cx + s * 0.27, cy - s * 0.24, s * 0.1);
    fill_round(cr, cx - s * 0.45, cy + s * 0.06, s * 0.9, s * 0.36, s * 0.18);
}

fn wifi_icon(cr: &Context, cx: f64, cy: f64, s: f64, strength: f64) {
    cr.set_line_width(s * 0.12);
    cr.set_line_cap(cairo::LineCap::Round);
    let base = cy + s * 0.35;
    for (i, r) in [0.3, 0.55, 0.8].iter().enumerate() {
        let on = strength >= (i as f64 + 1.0) / 4.0 || i == 0;
        let _ = cr.save();
        if !on {
            let (r0, g0, b0, a0) = (1.0, 1.0, 1.0, 0.3);
            cr.set_source_rgba(r0, g0, b0, a0);
        }
        cr.new_sub_path();
        cr.arc(cx, base, s * r, -PI * 0.78, -PI * 0.22);
        let _ = cr.stroke();
        let _ = cr.restore();
    }
    circle(cr, cx, base, s * 0.09);
}

fn bt_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    cr.set_line_width(s * 0.1);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    let (t, b, l, r) = (cy - s * 0.5, cy + s * 0.5, cx - s * 0.25, cx + s * 0.25);
    cr.move_to(l, cy - s * 0.25);
    cr.line_to(r, cy + s * 0.22);
    cr.line_to(cx, b);
    cr.line_to(cx, t);
    cr.line_to(r, cy - s * 0.22);
    cr.line_to(l, cy + s * 0.25);
    let _ = cr.stroke();
}

fn moon_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    // crescent: the disc minus an offset disc, clipped to the disc
    let _ = cr.save();
    cr.new_path();
    cr.arc(cx, cy, s * 0.45, 0.0, 2.0 * PI);
    cr.clip();
    cr.new_path();
    cr.arc(cx, cy, s * 0.45, 0.0, 2.0 * PI);
    cr.new_sub_path();
    cr.arc(cx + s * 0.24, cy - s * 0.2, s * 0.38, 0.0, 2.0 * PI);
    cr.set_fill_rule(cairo::FillRule::EvenOdd);
    let _ = cr.fill();
    cr.set_fill_rule(cairo::FillRule::Winding);
    let _ = cr.restore();
}

fn lock_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    fill_round(cr, cx - s * 0.35, cy - s * 0.05, s * 0.7, s * 0.5, s * 0.1);
    cr.set_line_width(s * 0.12);
    cr.new_sub_path();
    cr.arc(cx, cy - s * 0.08, s * 0.22, PI, 2.0 * PI);
    let _ = cr.stroke();
}

fn check_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    cr.set_line_width(s * 0.16);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.move_to(cx - s * 0.38, cy);
    cr.line_to(cx - s * 0.1, cy + s * 0.3);
    cr.line_to(cx + s * 0.4, cy - s * 0.32);
    let _ = cr.stroke();
}

fn headphones_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    cr.set_line_width(s * 0.11);
    cr.new_sub_path();
    cr.arc(cx, cy + s * 0.05, s * 0.4, PI, 2.0 * PI);
    let _ = cr.stroke();
    fill_round(cr, cx - s * 0.48, cy + s * 0.02, s * 0.2, s * 0.4, s * 0.06);
    fill_round(cr, cx + s * 0.28, cy + s * 0.02, s * 0.2, s * 0.4, s * 0.06);
}

fn chevron(cr: &Context, cx: f64, cy: f64, s: f64, left: bool) {
    let d = if left { -1.0 } else { 1.0 };
    cr.set_line_width(s * 0.16);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.move_to(cx - d * s * 0.15, cy - s * 0.35);
    cr.line_to(cx + d * s * 0.2, cy);
    cr.line_to(cx - d * s * 0.15, cy + s * 0.35);
    let _ = cr.stroke();
}

fn speaker_out_icon(cr: &Context, cx: f64, cy: f64, s: f64) {
    // the "choose output" button: a speaker with sound waves
    speaker_glyph(cr, cx - s * 0.45, cy, s * 0.8, 2, false);
}

// ------------------------------------------------------------ controls

pub const OUT_ROW: f64 = 34.0;
const ROW: f64 = 32.0;

fn back_rect(ix: f64) -> (f64, f64, f64, f64) {
    (ix + PAD - 8.0, HEADER + 4.0, 170.0, 30.0)
}

fn out_row(ix: f64, iw: f64, i: usize) -> (f64, f64, f64, f64) {
    (ix + PAD - 6.0, HEADER + 42.0 + i as f64 * OUT_ROW, iw - 2.0 * PAD + 12.0, OUT_ROW - 3.0)
}

fn tiles(ix: f64, iw: f64) -> [(f64, f64, f64, f64); 3] {
    let gap = 10.0;
    let tw = (iw - 2.0 * PAD - 2.0 * gap) / 3.0;
    std::array::from_fn(|i| (ix + PAD + i as f64 * (tw + gap), HEADER + 8.0, tw, 58.0))
}

fn slider(ix: f64, iw: f64, i: usize) -> (f64, f64, f64) {
    // (track x, track y, track width)
    (ix + PAD + 34.0, HEADER + 88.0 + i as f64 * 30.0, iw - 2.0 * PAD - 34.0 - 50.0)
}

fn detail_switch(ix: f64, iw: f64) -> (f64, f64, f64, f64) {
    (ix + iw - PAD - 42.0, HEADER + 10.0, 42.0, 22.0)
}

/// Rows of the open list (+ extra height for the password row).
pub fn detail_rows(m: &Model, ix: f64, iw: f64) -> Vec<(f64, f64, f64, f64)> {
    let mut y = HEADER + 44.0;
    let n = match m.detail {
        Some(Detail::Wifi) => m.nets.len().min(6),
        Some(Detail::Bluetooth) => m.bt_devs.len().min(6),
        None => 0,
    };
    (0..n)
        .map(|i| {
            let tall = m.detail == Some(Detail::Wifi) && m.password_for.as_deref() == Some(m.nets[i].ssid.as_str());
            let h = if tall { ROW + 36.0 } else { ROW };
            let r = (ix + PAD - 6.0, y, iw - 2.0 * PAD + 12.0, h - 2.0);
            y += h;
            r
        })
        .take_while(|r| r.1 + r.3 < HEADER + 246.0)
        .collect()
}

/// Where the GTK password entry goes (window coordinates) and the join button.
pub fn password_rects(m: &Model) -> Option<((f64, f64, f64, f64), (f64, f64, f64, f64))> {
    let (ix, iw, _, _) = island(m);
    let ssid = m.password_for.as_deref()?;
    let i = m.nets.iter().position(|n| n.ssid == ssid)?;
    let (x, y, w, _) = *detail_rows(m, ix, iw).get(i)?;
    Some(((x + 36.0, y + ROW, w - 36.0 - 96.0, 28.0), (x + w - 84.0, y + ROW, 76.0, 28.0)))
}

fn controls_hit(m: &Model, ix: f64, iw: f64, px: f64, py: f64) -> Hit {
    if let Some(d) = m.detail {
        if inside(back_rect(ix), px, py) {
            return Hit::Back;
        }
        if inside(detail_switch(ix, iw), px, py) {
            return Hit::DetailSwitch;
        }
        if let Some((_, join)) = password_rects(m) {
            if inside(join, px, py) {
                return Hit::Join;
            }
        }
        if let Some(i) = detail_rows(m, ix, iw).iter().position(|r| inside(*r, px, py)) {
            return if d == Detail::Wifi { Hit::NetRow(i) } else { Hit::BtRow(i) };
        }
        return Hit::Island;
    }
    let t = tiles(ix, iw);
    for (i, (x, y, w, h)) in t.iter().enumerate() {
        if inside((*x, *y, *w, *h), px, py) {
            let more = px > x + w - 30.0;
            return match i {
                0 if more => Hit::More(Detail::Wifi),
                0 => Hit::TileWifi,
                1 if more => Hit::More(Detail::Bluetooth),
                1 => Hit::TileBt,
                _ => Hit::TileDark,
            };
        }
    }
    for i in 0..2 {
        let (tx, ty, tw) = slider(ix, iw, i);
        if px >= tx - 8.0 && px <= tx + tw + 8.0 && (py - ty).abs() < 12.0 {
            let v = ((px - tx) / tw).clamp(0.0, 1.0);
            return if i == 0 { Hit::Volume(v) } else { Hit::Brightness(v) };
        }
    }
    Hit::Island
}

/// Slider value for a drag at x (for the hit being dragged).
pub fn slider_value(m: &Model, which: Hit, px: f64) -> Option<Hit> {
    let (ix, iw, _, _) = island(m);
    let i = match which {
        Hit::Volume(_) => 0,
        Hit::Brightness(_) => 1,
        _ => return None,
    };
    let (tx, _, tw) = slider(ix, iw, i);
    let v = ((px - tx) / tw).clamp(0.0, 1.0);
    Some(if i == 0 { Hit::Volume(v) } else { Hit::Brightness(v) })
}

fn draw_controls(cr: &Context, m: &Model, ix: f64, iw: f64, a: f64) {
    if let Some(d) = m.detail {
        return draw_detail(cr, m, d, ix, iw, a);
    }
    let c = &m.controls;
    let t = tiles(ix, iw);
    let tiles_info: [(bool, String, u8, bool); 3] = [
        (
            c.wifi == Some(true),
            match (c.wifi, &c.wifi_ssid) {
                (Some(false), _) => "Wi-Fi ปิด".into(),
                (_, Some(n)) => n.clone(),
                (None, _) => "Wi-Fi".into(),
                _ => "ไม่ได้เชื่อมต่อ".into(),
            },
            0,
            c.wifi.is_some(),
        ),
        (
            c.bluetooth == Some(true),
            match (c.bluetooth, &c.bt_device) {
                (Some(false), _) => "Bluetooth ปิด".into(),
                (_, Some(n)) => n.clone(),
                _ => "Bluetooth".into(),
            },
            1,
            c.bluetooth.is_some(),
        ),
        (c.dark == Some(true), if c.dark == Some(true) { "โหมดมืด".into() } else { "โหมดสว่าง".into() }, 2, false),
    ];
    for (i, (on, label, kind, more)) in tiles_info.iter().enumerate() {
        let (x, y, w, h) = t[i];
        if *on {
            rgba(cr, BLUE, a);
        } else {
            white(cr, 0.1 * a);
        }
        fill_round(cr, x, y, w, h, 14.0);
        white(cr, if *on { 1.0 } else { 0.8 } * a);
        let (icx, icy) = (x + w / 2.0, y + 20.0);
        match kind {
            0 => wifi_icon(cr, icx, icy, 18.0, 1.0),
            1 => bt_icon(cr, icx, icy, 18.0),
            _ => moon_icon(cr, icx, icy, 16.0),
        }
        white(cr, if *on { 1.0 } else { 0.7 } * a);
        text(cr, label, x + 8.0, y + 36.0, w - 16.0 - if *more { 18.0 } else { 0.0 }, 11.5, true, Align::Center);
        if *more {
            white(cr, 0.6 * a);
            chevron(cr, x + w - 14.0, y + h / 2.0, 12.0, false);
        }
    }
    // sliders
    let rows = [
        (c.volume.map(|v| if v.1 { 0.0 } else { v.0.min(1.0) }), 0usize),
        (c.brightness, 1usize),
    ];
    for (v, i) in rows {
        let (tx, ty, tw) = slider(ix, iw, i);
        white(cr, 0.85 * a);
        if i == 0 {
            speaker_glyph(cr, ix + PAD, ty, 14.0, 2, v == Some(0.0));
        } else {
            sun_glyph(cr, ix + PAD + 8.0, ty, 16.0);
        }
        white(cr, 0.18 * a);
        fill_round(cr, tx, ty - 2.5, tw, 5.0, 2.5);
        if let Some(v) = v {
            white(cr, 0.92 * a);
            fill_round(cr, tx, ty - 2.5, (tw * v).max(5.0), 5.0, 2.5);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.35 * a);
            circle(cr, tx + tw * v, ty, 8.5);
            white(cr, a);
            circle(cr, tx + tw * v, ty, 7.0);
            white(cr, 0.6 * a);
            mono(cr, &format!("{}%", (v * 100.0).round() as u32), ix + iw - PAD - 44.0, ty - 8.0, 44.0, 11.5, false, Align::Right);
        } else {
            white(cr, 0.4 * a);
            mono(cr, "—", ix + iw - PAD - 44.0, ty - 8.0, 44.0, 11.5, false, Align::Right);
        }
    }
}

fn draw_switch(cr: &Context, (x, y, w, h): (f64, f64, f64, f64), on: bool, a: f64) {
    if on {
        rgba(cr, BLUE, a);
    } else {
        white(cr, 0.22 * a);
    }
    fill_round(cr, x, y, w, h, h / 2.0);
    let k = h - 4.0;
    let kx = if on { x + w - k - 2.0 } else { x + 2.0 };
    white(cr, a);
    circle(cr, kx + k / 2.0, y + h / 2.0, k / 2.0);
}

fn draw_detail(cr: &Context, m: &Model, d: Detail, ix: f64, iw: f64, a: f64) {
    let (bx, by, _, bh) = back_rect(ix);
    white(cr, 0.8 * a);
    chevron(cr, bx + 12.0, by + bh / 2.0, 14.0, true);
    white(cr, a);
    text(cr, if d == Detail::Wifi { "Wi-Fi" } else { "Bluetooth" }, bx + 26.0, by + 5.0, 140.0, 15.0, true, Align::Left);
    let on = match d {
        Detail::Wifi => m.controls.wifi == Some(true),
        Detail::Bluetooth => m.controls.bluetooth == Some(true),
    };
    draw_switch(cr, detail_switch(ix, iw), on, a);
    if m.scanning && d == Detail::Wifi {
        white(cr, 0.5 * a);
        text(cr, "กำลังค้นหา…", ix + iw - PAD - 170.0, HEADER + 13.0, 120.0, 11.0, false, Align::Right);
    }
    white(cr, 0.08 * a);
    cr.rectangle(ix + PAD - 6.0, HEADER + 38.0, iw - 2.0 * PAD + 12.0, 1.0);
    let _ = cr.fill();
    let rows = detail_rows(m, ix, iw);
    let msg = |cr: &Context, s: &str| {
        white(cr, 0.45 * a);
        text(cr, s, ix, HEADER + 100.0, iw, 13.0, false, Align::Center);
    };
    if !on {
        return msg(cr, if d == Detail::Wifi { "Wi-Fi ปิดอยู่" } else { "Bluetooth ปิดอยู่" });
    }
    if rows.is_empty() {
        return msg(
            cr,
            match d {
                Detail::Wifi if m.scanning => "กำลังค้นหาเครือข่าย…",
                Detail::Wifi => "ไม่พบเครือข่าย",
                Detail::Bluetooth => "ยังไม่มีอุปกรณ์ที่จับคู่ไว้",
            },
        );
    }
    for (i, (x, y, w, _)) in rows.iter().enumerate() {
        let hovered = m.hover_hit == Some(if d == Detail::Wifi { Hit::NetRow(i) } else { Hit::BtRow(i) });
        let ty = y + (ROW - 2.0 - 16.0) / 2.0;
        match d {
            Detail::Wifi => {
                let n = &m.nets[i];
                if n.current || hovered {
                    white(cr, if n.current { 0.08 } else { 0.05 } * a);
                    fill_round(cr, *x, *y, *w, ROW - 2.0, 9.0);
                }
                white(cr, 0.9 * a);
                wifi_icon(cr, x + 18.0, y + 15.0, 15.0, n.signal as f64 / 100.0);
                white(cr, a);
                text(cr, &n.ssid, x + 36.0, ty, w - 36.0 - 140.0, 13.0, n.current, Align::Left);
                let rx = x + w - 136.0;
                if m.joining.as_deref() == Some(n.ssid.as_str()) {
                    white(cr, 0.6 * a);
                    text(cr, "กำลังเชื่อมต่อ…", rx, ty + 1.0, 128.0, 11.0, false, Align::Right);
                } else if n.current {
                    rgba(cr, BLUE, a);
                    check_icon(cr, x + w - 18.0, y + 15.0, 12.0);
                } else if m.join_failed.as_deref() == Some(n.ssid.as_str()) && m.password_for.is_none() {
                    rgba(cr, RED, a);
                    text(cr, "เชื่อมต่อไม่สำเร็จ", rx, ty + 1.0, 128.0, 11.0, false, Align::Right);
                } else if n.secure {
                    white(cr, 0.45 * a);
                    lock_icon(cr, x + w - 18.0, y + 15.0, 12.0);
                }
                if m.password_for.as_deref() == Some(n.ssid.as_str()) {
                    if let Some((_, (jx, jy, jw, jh))) = password_rects(m) {
                        rgba(cr, BLUE, a);
                        fill_round(cr, jx, jy, jw, jh, 8.0);
                        white(cr, a);
                        text(cr, "เข้าร่วม", jx, jy + 6.0, jw, 12.0, true, Align::Center);
                    }
                }
            }
            Detail::Bluetooth => {
                let dv = &m.bt_devs[i];
                if dv.connected || hovered {
                    white(cr, if dv.connected { 0.08 } else { 0.05 } * a);
                    fill_round(cr, *x, *y, *w, ROW - 2.0, 9.0);
                }
                if dv.connected {
                    rgba(cr, BLUE, a);
                } else {
                    white(cr, 0.8 * a);
                }
                if dv.audio {
                    headphones_icon(cr, x + 18.0, y + 15.0, 15.0);
                } else {
                    bt_icon(cr, x + 18.0, y + 15.0, 14.0);
                }
                white(cr, a);
                text(cr, &dv.name, x + 36.0, ty, w - 36.0 - 140.0, 13.0, false, Align::Left);
                let (label, col) = if m.bt_busy.as_deref() == Some(dv.mac.as_str()) {
                    ("กำลังดำเนินการ…", (1.0, 1.0, 1.0))
                } else if dv.connected {
                    ("เชื่อมต่อแล้ว", BLUE)
                } else {
                    ("ไม่ได้เชื่อมต่อ", (0.6, 0.6, 0.6))
                };
                rgba(cr, col, a);
                text(cr, label, x + w - 136.0, ty + 1.0, 128.0, 11.0, false, Align::Right);
            }
        }
    }
}

pub fn draw_outputs(cr: &Context, m: &Model, ix: f64, iw: f64, a: f64) {
    let (bx, by, _, bh) = back_rect(ix);
    white(cr, 0.8 * a);
    chevron(cr, bx + 12.0, by + bh / 2.0, 14.0, true);
    white(cr, a);
    text(cr, "เล่นเสียงที่", bx + 26.0, by + 5.0, 160.0, 15.0, true, Align::Left);
    white(cr, 0.08 * a);
    cr.rectangle(ix + PAD - 6.0, HEADER + 38.0, iw - 2.0 * PAD + 12.0, 1.0);
    let _ = cr.fill();
    if m.sinks.is_empty() {
        white(cr, 0.45 * a);
        text(cr, "ไม่พบอุปกรณ์เสียง (ต้องมี pactl)", ix, HEADER + 80.0, iw, 13.0, false, Align::Center);
        return;
    }
    for i in 0..m.sinks.len().min(6) {
        let (x, y, w, h) = out_row(ix, iw, i);
        let s = &m.sinks[i];
        if s.current || m.hover_hit == Some(Hit::OutRow(i)) {
            white(cr, if s.current { 0.1 } else { 0.06 } * a);
            fill_round(cr, x, y, w, h, 9.0);
        }
        if s.current {
            rgba(cr, BLUE, a);
        } else {
            white(cr, 0.85 * a);
        }
        let (icx, icy) = (x + 20.0, y + h / 2.0);
        if s.bluetooth {
            bt_icon(cr, icx, icy, 15.0);
        } else if s.headphones {
            headphones_icon(cr, icx, icy, 15.0);
        } else {
            speaker_glyph(cr, icx - 7.0, icy, 14.0, 2, false);
        }
        white(cr, a);
        text(cr, &s.description, x + 40.0, y + (h - 17.0) / 2.0, w - 40.0 - 120.0, 13.0, s.current, Align::Left);
        if s.current {
            rgba(cr, BLUE, a);
            check_icon(cr, x + w - 20.0, y + h / 2.0, 12.0);
        } else if s.bluetooth {
            white(cr, 0.4 * a);
            text(cr, "Bluetooth", x + w - 118.0, y + (h - 15.0) / 2.0, 108.0, 11.0, false, Align::Right);
        }
    }
}
