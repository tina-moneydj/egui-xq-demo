//! 顏色／字型：全部來自共用設計 token（gpui-xq-demo/crates/xq-tokens，W3C DTCG JSON），與 GPUI 版讀同一份。台股慣例：漲紅、跌綠。
//! 原本的 BG／UP… 常數改成同名小寫函式；換主題／熱更新後下一次呼叫就拿到新值。

use egui::Color32;

pub const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// 0xRRGGBB + alpha（0..1）
pub fn rgba(c: u32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied((c >> 16) as u8, (c >> 8) as u8, c as u8, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

pub use xq_tokens::t as tk;

/// token 顏色（含 alpha）→ Color32；與原本 from_rgba_unmultiplied(..., 0xNN) 逐位元相同
pub fn tok(c: xq_tokens::Rgba) -> Color32 {
    rgba(c.rgb, c.a)
}

pub fn bg() -> Color32 { rgb(tk().ui.bg) }
pub fn panel() -> Color32 { rgb(tk().ui.panel) }
pub fn header() -> Color32 { rgb(tk().ui.header) }
pub fn border() -> Color32 { rgb(tk().ui.border) }
pub fn text() -> Color32 { rgb(tk().ui.text) }
pub fn dim() -> Color32 { rgb(tk().ui.text_dim) }
pub fn up() -> Color32 { rgb(tk().market.up) }
pub fn down() -> Color32 { rgb(tk().market.down) }
pub fn flat() -> Color32 { rgb(tk().market.flat) }
pub fn accent() -> Color32 { rgb(tk().ui.accent) }
pub fn selected() -> Color32 { rgb(tk().ui.selected) }
pub fn vol() -> Color32 { rgb(tk().market.volume) }

pub fn dir_color(change: f64) -> Color32 {
    if change > 0.0 {
        up()
    } else if change < 0.0 {
        down()
    } else {
        flat()
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
