//! 顏色／字型常數（與 gpui-xq-demo theme.rs 同一套配色）。台股慣例：漲紅、跌綠。

use egui::Color32;

pub const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// 0xRRGGBB + alpha（0..1）
pub fn rgba(c: u32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied((c >> 16) as u8, (c >> 8) as u8, c as u8, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

pub const BG: Color32 = rgb(0x0b0e14);
pub const PANEL: Color32 = rgb(0x10141c);
pub const HEADER: Color32 = rgb(0x171c26);
pub const BORDER: Color32 = rgb(0x2a3140);
pub const TEXT: Color32 = rgb(0xd8dee9);
pub const DIM: Color32 = rgb(0x7f8a9e);
pub const UP: Color32 = rgb(0xff4d4f);
pub const DOWN: Color32 = rgb(0x22c55e);
pub const FLAT: Color32 = rgb(0xe5e7eb);
pub const ACCENT: Color32 = rgb(0x3b82f6);
pub const SELECTED: Color32 = rgb(0x1e3a5f);
pub const VOL: Color32 = rgb(0xf0c674);

pub fn dir_color(change: f64) -> Color32 {
    if change > 0.0 {
        UP
    } else if change < 0.0 {
        DOWN
    } else {
        FLAT
    }
}

pub fn fmt_int(v: i64) -> String {
    let s = v.unsigned_abs().to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3 + 1);
    if v < 0 {
        out.push('-');
    }
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn fmt_change(change: f64) -> String {
    if change > 0.0 {
        format!("▲{:.2}", change)
    } else if change < 0.0 {
        format!("▼{:.2}", -change)
    } else {
        "0.00".into()
    }
}

pub fn pct(price: f64, change: f64) -> f64 {
    let prev = price - change;
    if prev.abs() < 1e-9 { 0.0 } else { change / prev * 100.0 }
}
