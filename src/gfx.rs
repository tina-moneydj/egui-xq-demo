//! 低階繪圖：把一層要畫的東西收集成 egui Shape／Mesh，再用 epaint Tessellator「自己」三角化一次，
//! 得到可重複使用的 Mesh（Arc）。沒變的層下一幀直接把同一個 Arc<Mesh> 交給 egui，
//! 不重新產生 Shape、不重新排字、不重新三角化（egui 的立即模式預設是每幀全部重來）。
//!
//! 順序：填色（軸向矩形、K 棒、通道底色；直接寫進 Mesh，不做反鋸齒）→ 線（egui 線段，含 feathering 反鋸齒）→ 字。

use std::sync::Arc;

use egui::epaint::{Mesh, Shape, Stroke, TessellationOptions, Tessellator};
use egui::{pos2, Color32, Context, FontFamily, FontId, Pos2, Rect};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct P {
    pub x: f32,
    pub y: f32,
}

pub const fn p(x: f32, y: f32) -> P {
    P { x, y }
}

#[inline]
pub fn pos(q: P) -> Pos2 {
    pos2(q.x, q.y)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

struct Txt {
    s: String,
    x: f32,
    y_mid: f32,
    size: f32,
    color: Color32,
    align: Align,
    mono: bool,
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// 一層的圖元收集器。
/// 預設順序：整層的填色 → 線 → 字（批次最少）。呼叫 seal() 會切出一段：
/// finish 時一段一段依序畫（每段內 填色 → 線 → 字），畫線層用它做到「每條畫線嚴格依 z 順序」，
/// 只記邊界索引，不多配置 Mesh。
#[derive(Default)]
pub struct Sink {
    fill: Mesh,
    shapes: Vec<Shape>,
    texts: Vec<Txt>,
    /// 已切好的段：(fill 頂點結尾, fill 索引結尾, shapes 結尾, texts 結尾)
    cuts: Vec<(usize, usize, usize, usize)>,
}

impl Sink {
    pub fn is_empty(&self) -> bool {
        self.fill.is_empty() && self.shapes.is_empty() && self.texts.is_empty()
    }

    /// 切一段：之前加的圖元會在之後加的之前畫完
    pub fn seal(&mut self) {
        let c = (self.fill.vertices.len(), self.fill.indices.len(), self.shapes.len(), self.texts.len());
        if self.cuts.last() != Some(&c) {
            self.cuts.push(c);
        }
    }

    /// 軸向矩形（1px 線、K 棒、量柱）
    #[inline]
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Color32) {
        if !(w > 0.0 && h > 0.0) || !x.is_finite() || !y.is_finite() {
            return;
        }
        self.fill.add_colored_rect(Rect::from_min_size(pos2(x, y), egui::vec2(w, h)), c);
    }

    /// 帶邊框的矩形
    pub fn boxed(&mut self, x: f32, y: f32, w: f32, h: f32, bg: Color32, border: Color32, bw: f32) {
        self.rect(x, y, w, h, bg);
        self.rect(x, y, w, bw, border);
        self.rect(x, y + h - bw, w, bw, border);
        self.rect(x, y, bw, h, border);
        self.rect(x + w - bw, y, bw, h, border);
    }

    #[inline]
    pub fn tri(&mut self, a: P, b: P, c: P, col: Color32) {
        let i = self.fill.vertices.len() as u32;
        self.fill.colored_vertex(pos(a), col);
        self.fill.colored_vertex(pos(b), col);
        self.fill.colored_vertex(pos(c), col);
        self.fill.add_triangle(i, i + 1, i + 2);
    }

    /// 凸多邊形填色（扇形）
    pub fn poly(&mut self, pts: &[P], col: Color32) {
        for k in 1..pts.len().saturating_sub(1) {
            self.tri(pts[0], pts[k], pts[k + 1], col);
        }
    }

    #[inline]
    pub fn seg(&mut self, a: P, b: P, w: f32, c: Color32) {
        if a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite() {
            self.shapes.push(Shape::line_segment([pos(a), pos(b)], Stroke::new(w, c)));
        }
    }

    /// 折線：None／NaN 斷開
    pub fn polyline(&mut self, w: f32, c: Color32, pts: impl Iterator<Item = Option<P>>) {
        let mut run: Vec<Pos2> = Vec::new();
        let flush = |run: &mut Vec<Pos2>, shapes: &mut Vec<Shape>| {
            if run.len() >= 2 {
                shapes.push(Shape::line(std::mem::take(run), Stroke::new(w, c)));
            } else {
                run.clear();
            }
        };
        for q in pts {
            match q {
                Some(q) if q.x.is_finite() && q.y.is_finite() => run.push(pos(q)),
                _ => flush(&mut run, &mut self.shapes),
            }
        }
        flush(&mut run, &mut self.shapes);
    }

    pub fn dashed(&mut self, a: P, b: P, w: f32, c: Color32, dash: f32, gap: f32) {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = (dx * dx + dy * dy).sqrt();
        if !(len > 1e-3) || !len.is_finite() {
            return;
        }
        let (ux, uy) = (dx / len, dy / len);
        let mut t = 0.0;
        while t < len {
            let e = (t + dash).min(len);
            self.seg(p(a.x + ux * t, a.y + uy * t), p(a.x + ux * e, a.y + uy * e), w, c);
            t += dash + gap;
        }
    }

    /// 水平／垂直虛線（矩形，不需反鋸齒）
    pub fn hdash(&mut self, x0: f32, x1: f32, y: f32, c: Color32, dash: f32, gap: f32) {
        let mut x = x0;
        while x < x1 {
            self.rect(x, y, dash.min(x1 - x), 1.0, c);
            x += dash + gap;
        }
    }
    pub fn vdash(&mut self, x: f32, y0: f32, y1: f32, c: Color32, dash: f32, gap: f32) {
        let mut y = y0;
        while y < y1 {
            self.rect(x, y, 1.0, dash.min(y1 - y), c);
            y += dash + gap;
        }
    }

    pub fn dot(&mut self, x: f32, y: f32, r: f32, c: Color32) {
        // 小點用正方形近似（SAR 點 r≈1.2–2.5px，與圓幾乎看不出差別，頂點數 4 vs 圓的數十個）
        self.rect(x - r, y - r, 2.0 * r, 2.0 * r, c);
    }

    pub fn text(&mut self, s: impl Into<String>, x: f32, y_mid: f32, size: f32, color: Color32) {
        self.texts.push(Txt { s: s.into(), x, y_mid, size, color, align: Align::Left, mono: true });
    }
    pub fn text_at(&mut self, s: impl Into<String>, x: f32, y_mid: f32, size: f32, color: Color32, align: Align) {
        self.texts.push(Txt { s: s.into(), x, y_mid, size, color, align, mono: true });
    }
    /// 介面字型（非等寬，代號／名稱欄）
    pub fn text_ui(&mut self, s: impl Into<String>, x: f32, y_mid: f32, size: f32, color: Color32, align: Align) {
        self.texts.push(Txt { s: s.into(), x, y_mid, size, color, align, mono: false });
    }

    /// 三角化成一個 Mesh（字的 UV 依目前字型貼圖大小；貼圖變大時呼叫端要重建快取）
    pub fn finish(mut self, ctx: &Context) -> Mesh {
        if !self.cuts.is_empty() {
            self.seal_end();
        }
        let galleys: Vec<(Arc<egui::Galley>, &Txt)> = if self.texts.is_empty() {
            Vec::new()
        } else {
            ctx.fonts_mut(|f| self.texts.iter().map(|t| (f.layout_no_wrap(t.s.clone(), if t.mono { mono(t.size) } else { FontId::proportional(t.size) }, t.color), t)).collect())
        };
        let mut tess = tessellator(ctx);
        let put_text = |tess: &mut Tessellator, g: Arc<egui::Galley>, t: &Txt, out: &mut Mesh| {
            let sz = g.size();
            let x = match t.align {
                Align::Left => t.x,
                Align::Center => t.x - sz.x / 2.0,
                Align::Right => t.x - sz.x,
            };
            let y = t.y_mid - sz.y / 2.0;
            tess.tessellate_shape(Shape::galley(pos2(x.round(), y.round()), g, t.color), out);
        };
        if self.cuts.is_empty() {
            let mut fill = std::mem::take(&mut self.fill);
            for s in std::mem::take(&mut self.shapes) {
                tess.tessellate_shape(s, &mut fill);
            }
            for (g, t) in galleys {
                put_text(&mut tess, g, t, &mut fill);
            }
            return fill;
        }
        // 分段：每段依序 填色 → 線 → 字
        let mut out = Mesh::default();
        out.vertices.reserve(self.fill.vertices.len() * 2);
        out.indices.reserve(self.fill.indices.len() * 2);
        let mut galleys = galleys.into_iter();
        let mut shapes = std::mem::take(&mut self.shapes).into_iter();
        let (mut v0, mut i0, mut s0, mut t0) = (0, 0, 0, 0);
        for &(v1, i1, s1, t1) in &self.cuts {
            let base = out.vertices.len() as u32;
            out.vertices.extend_from_slice(&self.fill.vertices[v0..v1]);
            out.indices.extend(self.fill.indices[i0..i1].iter().map(|&i| i - v0 as u32 + base));
            for s in shapes.by_ref().take(s1 - s0) {
                tess.tessellate_shape(s, &mut out);
            }
            for (g, t) in galleys.by_ref().take(t1 - t0) {
                put_text(&mut tess, g, t, &mut out);
            }
            (v0, i0, s0, t0) = (v1, i1, s1, t1);
        }
        out
    }

    fn seal_end(&mut self) {
        self.seal();
    }
}

/// XQ_NO_VIEW_CACHE（與 gpui 版同名）：關掉「沒變就沿用」的 Mesh 快取——走勢圖底圖／即時層、報價表可見列、
/// 成交明細可見列每幀都重建（gpui 版是關掉 `.cached()` 的 view 快取，每幀重新 render／paint）。量快取省了多少用。
pub fn no_view_cache() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| crate::plat::var_os("XQ_NO_VIEW_CACHE").is_some())
}

/// XQ_LAYER_MODE=split（與 gpui 版同名）：走勢圖三層（底圖／畫線／即時）各畫在自己的 egui 子圖層
/// （`set_sublayer`，緊接在主圖層之後合成）；預設 single＝全部畫在同一個圖層。
/// egui 每個圖層×裁切區各一批 draw call，所以 split 會多幾批，但不像 gpui 那樣多清 MSAA 中介貼圖。
pub fn split_layers() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| crate::plat::var("XQ_LAYER_MODE").as_deref() == Ok("split"))
}

pub fn tessellator(ctx: &Context) -> Tessellator {
    let opts: TessellationOptions = ctx.tessellation_options(|o| *o);
    let tex = ctx.fonts(|f| f.font_image_size());
    Tessellator::new(ctx.pixels_per_point(), opts, tex, Vec::new())
}

/// 字型貼圖大小＋縮放（快取鍵的一部分：貼圖長大後舊 Mesh 的字 UV 會錯）
pub fn font_key(ctx: &Context) -> ([usize; 2], u32) {
    (ctx.fonts(|f| f.font_image_size()), ctx.pixels_per_point().to_bits())
}

/// 粗略字寬（等寬字：ASCII 0.6em、CJK 1.2em，與 gpui 版相同）
pub fn text_width(s: &str, size: f32) -> f32 {
    s.chars().map(|c| if c.is_ascii() { 0.6 } else { 1.2 }).sum::<f32>() * size
}
