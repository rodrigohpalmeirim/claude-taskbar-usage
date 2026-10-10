//! Draws the widget into a premultiplied 32-bit DIB for UpdateLayeredWindow.
//!
//! Shapes are rasterised here with 4x4 supersampling. Text is drawn by GDI
//! in white on black into a second DIB whose brightness is then used as
//! coverage, which gives grayscale-antialiased text that blends correctly
//! over the translucent taskbar.

use super::{from_wide, wide};
use crate::config::Config;
use crate::timefmt::{self, Locale};
use crate::usage::{Quota, Usage};
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rgba(u8, u8, u8, u8);

/// Colors for one taskbar theme.
struct Palette {
    text: Rgba,
    dim: Rgba,
    track: Rgba,
    fill: Rgba,
    warn: Rgba,
    tick: Rgba,
    marker: Rgba,
    hover: Rgba,
}

/// Dark taskbar, light text.
const DARK: Palette = Palette {
    text: Rgba(255, 255, 255, 240),
    dim: Rgba(255, 255, 255, 160),
    track: Rgba(255, 255, 255, 48),
    fill: Rgba(74, 158, 255, 255),
    warn: Rgba(232, 88, 88, 255),
    tick: Rgba(0, 0, 0, 170),
    marker: Rgba(255, 255, 255, 255),
    hover: Rgba(255, 255, 255, 18),
};

/// Light taskbar, dark text.
const LIGHT: Palette = Palette {
    text: Rgba(0, 0, 0, 230),
    dim: Rgba(0, 0, 0, 150),
    track: Rgba(0, 0, 0, 40),
    fill: Rgba(0, 103, 192, 255),
    warn: Rgba(196, 43, 28, 255),
    tick: Rgba(255, 255, 255, 210),
    marker: Rgba(0, 0, 0, 235),
    hover: Rgba(0, 0, 0, 14),
};

/// Everything that determines what the widget looks like.
pub struct View<'a> {
    pub usage: Option<&'a Usage>,
    pub now: i64,
    /// Shown instead of the bars when there is no data.
    pub msg: &'a str,
    pub stale: bool,
    pub hover: bool,
    pub light: bool,
    pub cfg: &'a Config,
    pub locale: &'a dyn Locale,
}

/// Pre-formatted strings for one bar.
struct Row<'a> {
    label: &'static str,
    quota: Option<&'a Quota>,
    pct: String,
    countdown: String,
    reset_at: String,
}

fn rows<'a>(v: &View<'a>, usage: &'a Usage) -> Vec<Row<'a>> {
    let make = |label, quota: Option<&'a Quota>| {
        let mut r = Row { label, quota, pct: "–".into(), countdown: "–".into(), reset_at: String::new() };
        if let Some(q) = quota {
            r.pct = format_percent(q.percent);
            match q.resets_at {
                Some(t) => {
                    r.countdown = timefmt::countdown(t - v.now);
                    r.reset_at = timefmt::reset_at(t, v.now, v.locale);
                }
                None => r.reset_at = "not started".into(),
            }
        }
        r
    };
    let mut out = Vec::with_capacity(2);
    if v.cfg.show_session {
        out.push(make("5h", usage.session.as_ref()));
    }
    if v.cfg.show_weekly {
        out.push(make("7d", usage.weekly.as_ref()));
    }
    out
}

pub fn format_percent(p: f64) -> String {
    format!("{}%", p.round() as i64)
}

/// Column positions in device pixels.
pub struct Layout {
    pub w: i32,
    pub h: i32,
    pad: f64,
    label: (f64, f64),
    bar: (f64, f64),
    pct: (f64, f64),
    countdown: (f64, f64),
    reset: (f64, f64),
    row_y: Vec<f64>,
}

struct TextItem<'a> {
    x0: f64,
    x1: f64,
    cy: f64,
    s: &'a str,
    bold: bool,
    align: DRAW_TEXT_FORMAT,
    dim: bool,
}

fn text_item(col: (f64, f64), cy: f64, s: &str, bold: bool, align: DRAW_TEXT_FORMAT, dim: bool) -> TextItem<'_> {
    TextItem { x0: col.0, x1: col.0 + col.1, cy, s, bold, align, dim }
}

pub struct Renderer {
    pub scale: f64,
    font: HFONT,
    bold: HFONT,
    face: Vec<u16>,
    measure_dc: HDC,
    w: i32,
    h: i32,
    text: Surface,
    out: Surface,
}

/// A memory DC with a selected 32-bit top-down DIB.
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    len: usize,
}

impl Surface {
    const EMPTY: Surface =
        Surface { dc: std::ptr::null_mut(), bmp: std::ptr::null_mut(), old: std::ptr::null_mut(), bits: std::ptr::null_mut(), len: 0 };

    fn new(w: i32, h: i32) -> Surface {
        // SAFETY: standard DIB section creation; the bitmap stays selected
        // into the DC until `release`.
        unsafe {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                },
                ..std::mem::zeroed()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(std::ptr::null_mut(), &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            let old = SelectObject(dc, bmp);
            Surface { dc, bmp, old, bits: bits.cast(), len: if bits.is_null() { 0 } else { (w * h) as usize } }
        }
    }

    fn pixels(&mut self) -> &mut [u32] {
        if self.bits.is_null() {
            return &mut [];
        }
        // SAFETY: the DIB holds exactly `len` u32 pixels while selected.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
    }

    fn release(&mut self) {
        if self.dc.is_null() {
            return;
        }
        // SAFETY: undoes `new` in reverse order.
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
        *self = Surface::EMPTY;
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.text.release();
        self.out.release();
        // SAFETY: objects owned by this renderer.
        unsafe {
            DeleteObject(self.font);
            DeleteObject(self.bold);
            DeleteDC(self.measure_dc);
        }
    }
}

impl Renderer {
    pub fn new() -> Renderer {
        // SAFETY: plain GDI object creation.
        let measure_dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        let mut r = Renderer {
            scale: 0.0,
            font: std::ptr::null_mut(),
            bold: std::ptr::null_mut(),
            face: pick_font_face(),
            measure_dc,
            w: 0,
            h: 0,
            text: Surface::EMPTY,
            out: Surface::EMPTY,
        };
        r.set_scale(1.0);
        r
    }

    pub fn set_scale(&mut self, scale: f64) {
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        let px = (12.0 * scale).round() as i32;
        // SAFETY: replaces fonts owned by this renderer.
        unsafe {
            if !self.font.is_null() {
                DeleteObject(self.font);
                DeleteObject(self.bold);
            }
            self.font = create_font(px, FW_NORMAL as i32, &self.face);
            self.bold = create_font(px, FW_SEMIBOLD as i32, &self.face);
        }
    }

    fn text_width(&self, s: &str, bold: bool) -> f64 {
        if s.is_empty() {
            return 0.0;
        }
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        // SAFETY: font is a live GDI object; the string length is exact.
        unsafe {
            let old = SelectObject(self.measure_dc, if bold { self.bold } else { self.font });
            GetTextExtentPoint32W(self.measure_dc, w.as_ptr(), w.len() as i32, &mut size);
            SelectObject(self.measure_dc, old);
        }
        f64::from(size.cx)
    }

    pub fn layout(&self, v: &View, height: i32) -> Layout {
        let s = self.scale;
        let c = v.cfg;
        let pad = (8.0 * s).round();
        let mut l = Layout {
            w: 0,
            h: height,
            pad,
            label: (0.0, 0.0),
            bar: (0.0, 0.0),
            pct: (0.0, 0.0),
            countdown: (0.0, 0.0),
            reset: (0.0, 0.0),
            row_y: Vec::new(),
        };
        let Some(usage) = v.usage else {
            l.w = (pad * 2.0 + self.text_width(v.msg, false)).ceil() as i32;
            return l;
        };
        let rows = rows(v, usage);

        // Columns go left to right; each is preceded by its gap unless it
        // is the first visible one.
        let mut x = pad;
        let mut first = true;
        let mut column = |show: bool, gap: f64, width: f64| -> (f64, f64) {
            if !show {
                return (0.0, 0.0);
            }
            if !first {
                x += (gap * s).round();
            }
            first = false;
            let start = x;
            x += width;
            (start, width)
        };

        let label_w = if c.show_labels { self.text_width("5h", false).max(self.text_width("7d", false)) } else { 0.0 };
        l.label = column(c.show_labels, 0.0, label_w);
        l.bar = column(c.show_bar, 6.0, (f64::from(c.bar_width) * s).round());
        let pct_w = if c.show_percent { self.text_width("100%", true) } else { 0.0 };
        l.pct = column(c.show_percent, 6.0, pct_w);
        let mut cd_w = 0.0;
        if c.show_countdown {
            cd_w = self.text_width("23h 59m", false).max(self.text_width("6d 23h", false));
            for r in &rows {
                cd_w = f64::max(cd_w, self.text_width(&r.countdown, false));
            }
        }
        l.countdown = column(c.show_countdown, 9.0, cd_w);
        let mut rs_w = 0.0;
        if c.show_reset_time {
            // Reserve room for a weekday + late-evening time ("Wed 22:58") so
            // the width doesn't jump as the reset moves between days.
            let now = timefmt::days_from_civil(2026, 9, 2) * timefmt::DAY;
            let sample = now + 7 * timefmt::DAY + 22 * timefmt::HOUR + 58 * timefmt::MINUTE;
            rs_w = self.text_width(&timefmt::reset_at(sample, now, v.locale), false);
            for r in &rows {
                rs_w = f64::max(rs_w, self.text_width(&r.reset_at, false));
            }
        }
        l.reset = column(c.show_reset_time, 5.0, rs_w);
        l.w = (x + pad).ceil() as i32;

        let mid = f64::from(height) / 2.0;
        l.row_y = if rows.len() == 1 { vec![mid] } else { vec![mid - (9.0 * s).round(), mid + (9.0 * s).round()] };
        l
    }

    fn ensure_buffers(&mut self, w: i32, h: i32) {
        if w == self.w && h == self.h && !self.out.dc.is_null() {
            return;
        }
        self.text.release();
        self.out.release();
        self.w = w;
        self.h = h;
        self.text = Surface::new(w, h);
        self.out = Surface::new(w, h);
        // SAFETY: configures the text DC we own.
        unsafe {
            SetBkMode(self.text.dc, TRANSPARENT as i32);
            SetTextColor(self.text.dc, 0x00FF_FFFF);
        }
    }

    /// Renders the view; returns the DC holding the finished bitmap.
    pub fn draw(&mut self, v: &View, l: &Layout) -> HDC {
        self.ensure_buffers(l.w, l.h);
        let pal = if v.light { &LIGHT } else { &DARK };
        self.out.pixels().fill(0);
        let s = self.scale;
        let (w, h) = (f64::from(l.w), f64::from(l.h));

        if v.hover {
            let inset = (3.0 * s).round();
            self.fill_shape(1.0, inset, w - 1.0, h - inset, 4.0 * s, w, pal.hover);
        }

        let mut texts: Vec<TextItem> = Vec::new();
        let rows = v.usage.map(|u| rows(v, u)).unwrap_or_default();
        if v.usage.is_none() {
            texts.push(TextItem { x0: l.pad, x1: w - l.pad, cy: h / 2.0, s: v.msg, bold: false, align: DT_LEFT, dim: true });
        }
        let c = v.cfg;
        for (row, &cy) in rows.iter().zip(&l.row_y) {
            let text = |col, s, bold, align, dim| text_item(col, cy, s, bold, align, dim);
            if c.show_labels {
                texts.push(text(l.label, row.label, false, DT_LEFT, true));
            }
            if c.show_bar {
                self.draw_bar(row.quota, v, l.bar.0, cy, l.bar.1, pal);
            }
            if c.show_percent {
                texts.push(text(l.pct, &row.pct, true, DT_RIGHT, false));
            }
            if c.show_countdown {
                texts.push(text(l.countdown, &row.countdown, false, DT_LEFT, false));
            }
            if c.show_reset_time {
                texts.push(text(l.reset, &row.reset_at, false, DT_LEFT, true));
            }
        }
        self.draw_texts(&texts, false, pal.text);
        self.draw_texts(&texts, true, pal.dim);

        // Stale data is shown faded. Every pixel keeps a minimal alpha so
        // the whole rectangle stays clickable (fully transparent pixels let
        // mouse input fall through to the taskbar).
        let fade = if v.stale { 0.5 } else { 1.0 };
        for p in self.out.pixels() {
            if fade != 1.0 {
                *p = scale_pixel(*p, fade);
            }
            if *p >> 24 == 0 {
                *p = 0x0100_0000;
            }
        }
        self.out.dc
    }

    fn draw_bar(&mut self, q: Option<&Quota>, v: &View, x: f64, cy: f64, w: f64, pal: &Palette) {
        let s = self.scale;
        let h = (6.0 * s).round().max(4.0);
        let y = (cy - h / 2.0).round();
        let rad = h / 2.0;
        self.fill_shape(x, y, x + w, y + h, rad, x + w, pal.track);
        let Some(q) = q else { return };
        let fill = if v.cfg.warn_when_outpacing && timefmt::outpacing(q, v.now) { pal.warn } else { pal.fill };
        let pct = q.percent.clamp(0.0, 100.0);
        if pct > 0.0 {
            self.fill_shape(x, y, x + w, y + h, rad, x + w * pct / 100.0, fill);
        }
        if v.cfg.show_ticks {
            let tw = s.round().max(1.0);
            for t in timefmt::ticks(q, v.locale) {
                let tx = (x + w * t - tw / 2.0).round();
                self.fill_shape(tx, y, tx + tw, y + h, 0.0, tx + tw, pal.tick);
            }
        }
        if v.cfg.show_marker
            && let Some(f) = timefmt::elapsed_fraction(q, v.now)
        {
            let mw = (2.0 * s).round().max(2.0);
            let ext = (3.0 * s).round();
            let mx = (x + w * f - mw / 2.0).round().clamp(x - mw / 2.0, x + w - mw / 2.0);
            self.fill_shape(mx, y - ext, mx + mw, y + h + ext, mw / 2.0, mx + mw, pal.marker);
        }
    }

    /// Fills a rounded rectangle, clipped at `clip_x`, with 4x4 supersampled coverage.
    #[allow(clippy::too_many_arguments)]
    fn fill_shape(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, rad: f64, clip_x: f64, c: Rgba) {
        let (w, h) = (self.w, self.h);
        let ix0 = x0.floor().max(0.0) as i32;
        let iy0 = y0.floor().max(0.0) as i32;
        let ix1 = (x1.min(clip_x).ceil() as i32).min(w);
        let iy1 = (y1.ceil() as i32).min(h);
        let inside = |sx: f64, sy: f64| {
            if sx < x0 || sx >= x1 || sx >= clip_x || sy < y0 || sy >= y1 {
                return false;
            }
            if rad <= 0.0 {
                return true;
            }
            let dx = sx - sx.clamp(x0 + rad, x1 - rad);
            let dy = sy - sy.clamp(y0 + rad, y1 - rad);
            dx * dx + dy * dy <= rad * rad
        };
        let px = self.out.pixels();
        for py in iy0..iy1 {
            for pxx in ix0..ix1 {
                let mut n = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        if inside(f64::from(pxx) + (f64::from(sx) + 0.5) / 4.0, f64::from(py) + (f64::from(sy) + 0.5) / 4.0) {
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    let i = (py * w + pxx) as usize;
                    px[i] = blend(px[i], c, f64::from(n) / 16.0);
                }
            }
        }
    }

    /// Renders all items of one color through the GDI coverage buffer.
    fn draw_texts(&mut self, items: &[TextItem], dim: bool, color: Rgba) {
        let items: Vec<&TextItem> = items.iter().filter(|t| t.dim == dim && !t.s.is_empty()).collect();
        if items.is_empty() {
            return;
        }
        self.text.pixels().fill(0);
        let half = (10.0 * self.scale).round();
        for t in items {
            let mut rc = RECT {
                left: t.x0.round() as i32,
                top: (t.cy - half).round() as i32,
                right: t.x1.round() as i32,
                bottom: (t.cy + half).round() as i32,
            };
            let mut ws: Vec<u16> = t.s.encode_utf16().collect();
            // SAFETY: font and DC are live; the string length is exact.
            unsafe {
                let old = SelectObject(self.text.dc, if t.bold { self.bold } else { self.font });
                DrawTextW(
                    self.text.dc,
                    ws.as_mut_ptr(),
                    ws.len() as i32,
                    &mut rc,
                    t.align | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP,
                );
                SelectObject(self.text.dc, old);
            }
        }
        // SAFETY: makes GDI finish writing the DIB before we read it.
        unsafe { GdiFlush() };
        let len = self.text.len.min(self.out.len);
        for i in 0..len {
            // SAFETY: both DIBs have at least `len` pixels; distinct buffers.
            let (p, dst) = unsafe { (*self.text.bits.add(i), &mut *self.out.bits.add(i)) };
            if p & 0x00FF_FFFF == 0 {
                continue;
            }
            let cov = f64::from(((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF)) / (3.0 * 255.0);
            *dst = blend(*dst, color, cov);
        }
    }
}

fn create_font(px: i32, weight: i32, face: &[u16]) -> HFONT {
    // SAFETY: face is NUL-terminated.
    unsafe { CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, ANTIALIASED_QUALITY as u32, 0, face.as_ptr()) }
}

/// Prefers the Windows 11 taskbar font and falls back to Segoe UI.
fn pick_font_face() -> Vec<u16> {
    let want = "Segoe UI Variable Text";
    let face = wide(want);
    // SAFETY: temporary font and DC, released before returning.
    let got = unsafe {
        let font = create_font(12, FW_NORMAL as i32, &face);
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let old = SelectObject(dc, font);
        let mut buf = [0u16; 64];
        GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
        SelectObject(dc, old);
        DeleteDC(dc);
        DeleteObject(font);
        from_wide(&buf)
    };
    if got.eq_ignore_ascii_case(want) { face } else { wide("Segoe UI") }
}

/// Composites a straight-alpha color with coverage over a premultiplied BGRA pixel.
fn blend(dst: u32, c: Rgba, cov: f64) -> u32 {
    let a = f64::from(c.3) / 255.0 * cov;
    if a <= 0.0 {
        return dst;
    }
    let inv = 1.0 - a;
    let ch = |shift: u32, src: u8| -> u32 { clamp255(f64::from(src) * a + f64::from((dst >> shift) & 0xFF) * inv) << shift };
    let alpha = clamp255(255.0 * a + f64::from(dst >> 24) * inv) << 24;
    alpha | ch(16, c.0) | ch(8, c.1) | ch(0, c.2)
}

fn scale_pixel(p: u32, f: f64) -> u32 {
    let ch = |shift: u32| clamp255(f64::from((p >> shift) & 0xFF) * f) << shift;
    ch(24) | ch(16) | ch(8) | ch(0)
}

fn clamp255(v: f64) -> u32 {
    v.round().clamp(0.0, 255.0) as u32
}
