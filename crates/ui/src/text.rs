//! Text rasterisation with the bundled Inter font (SIL OFL 1.1, see `assets/OFL.txt`).
//!
//! Glyph coverage bitmaps are cached per (weight, char, pixel size), so steady-state rendering of
//! already-seen strings does not allocate.

use std::collections::HashMap;
use std::sync::LazyLock;

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::Pixmap;

use crate::draw::Rgba;

static REGULAR_TTF: &[u8] = include_bytes!("../assets/Inter-Regular.ttf");
static SEMIBOLD_TTF: &[u8] = include_bytes!("../assets/Inter-SemiBold.ttf");

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Weight {
    Regular,
    SemiBold,
}

struct Fonts {
    regular: Font,
    semibold: Font,
}

static FONTS: LazyLock<Fonts> = LazyLock::new(|| {
    let load = |bytes: &'static [u8]| {
        Font::from_bytes(bytes, FontSettings::default()).expect("bundled Inter font parses")
    };
    Fonts {
        regular: load(REGULAR_TTF),
        semibold: load(SEMIBOLD_TTF),
    }
});

fn font(weight: Weight) -> &'static Font {
    match weight {
        Weight::Regular => &FONTS.regular,
        Weight::SemiBold => &FONTS.semibold,
    }
}

/// Sizes are quantised to quarter pixels so the cache key is exact.
fn size_key(px: f32) -> u32 {
    (px * 4.0).round().max(1.0) as u32
}

struct Glyph {
    metrics: Metrics,
    coverage: Vec<u8>,
}

/// Horizontal alignment relative to the x coordinate passed to [`TextRenderer::draw`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Align {
    Left,
    Center,
    Right,
}

/// Rasterises and caches glyphs; draws text onto premultiplied pixmaps.
#[derive(Default)]
pub(crate) struct TextRenderer {
    cache: HashMap<(Weight, char, u32), Glyph>,
}

impl TextRenderer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn glyph(&mut self, weight: Weight, ch: char, px: f32) -> &Glyph {
        let key = (weight, ch, size_key(px));
        self.cache.entry(key).or_insert_with(|| {
            let (metrics, coverage) = font(weight).rasterize(ch, key.2 as f32 / 4.0);
            Glyph { metrics, coverage }
        })
    }

    /// Height of capital letters / digits above the baseline (used for optical centring).
    pub(crate) fn cap_height(weight: Weight, px: f32) -> f32 {
        let m = font(weight).metrics('H', px);
        m.height as f32 + m.ymin as f32
    }

    /// Advance width of `text` in pixels.
    pub(crate) fn measure(&mut self, weight: Weight, px: f32, text: &str) -> f32 {
        let f = font(weight);
        let mut width = 0.0;
        let mut prev: Option<char> = None;
        for ch in text.chars() {
            if let Some(p) = prev {
                width += f.horizontal_kern(p, ch, px).unwrap_or(0.0);
            }
            width += self.glyph(weight, ch, px).metrics.advance_width;
            prev = Some(ch);
        }
        width
    }

    /// Largest size in `[min_px, max_px]` (0.5 px steps) at which `text` fits `max_width`.
    /// Returns `min_px` when even that does not fit (callers clip or wrap in that case).
    pub(crate) fn fit(
        &mut self,
        weight: Weight,
        max_px: f32,
        min_px: f32,
        max_width: f32,
        text: &str,
    ) -> f32 {
        let natural = self.measure(weight, max_px, text);
        if natural <= max_width {
            return max_px;
        }
        // Width scales almost linearly with size; start from the estimate and walk down.
        let mut px = (max_px * max_width / natural * 2.0).floor() / 2.0;
        px = px.clamp(min_px, max_px);
        while px > min_px && self.measure(weight, px, text) > max_width {
            px -= 0.5;
        }
        px.max(min_px)
    }

    /// Greedy word wrap. Returns byte ranges of `text` for each line.
    pub(crate) fn wrap(
        &mut self,
        weight: Weight,
        px: f32,
        max_width: f32,
        text: &str,
        out: &mut Vec<(usize, usize)>,
    ) {
        out.clear();
        for (para_start, para) in split_paragraphs(text) {
            let mut line_start: Option<usize> = None;
            let mut line_end = 0usize;
            for (ws, we) in words(para) {
                let (ws, we) = (para_start + ws, para_start + we);
                match line_start {
                    None => {
                        line_start = Some(ws);
                        line_end = we;
                    }
                    Some(ls) => {
                        if self.measure(weight, px, &text[ls..we]) <= max_width {
                            line_end = we;
                        } else {
                            out.push((ls, line_end));
                            line_start = Some(ws);
                            line_end = we;
                        }
                    }
                }
            }
            match line_start {
                Some(ls) => out.push((ls, line_end)),
                None => out.push((para_start, para_start)),
            }
        }
    }

    /// Draws `text` with its baseline at `baseline_y`. `x` is interpreted according to `align`.
    /// Pixels outside `clip` (x0, y0, x1, y1) are not touched. Returns the drawn width.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        pixmap: &mut Pixmap,
        weight: Weight,
        px: f32,
        x: f32,
        baseline_y: f32,
        align: Align,
        text: &str,
        color: Rgba,
        clip: Option<(i32, i32, i32, i32)>,
    ) -> f32 {
        let width = self.measure(weight, px, text);
        let mut pen = match align {
            Align::Left => x,
            Align::Center => x - width / 2.0,
            Align::Right => x - width,
        };
        let f = font(weight);
        let baseline = baseline_y.round() as i32;
        let mut prev: Option<char> = None;
        for ch in text.chars() {
            if let Some(p) = prev {
                pen += f.horizontal_kern(p, ch, px).unwrap_or(0.0);
            }
            let glyph = self.glyph(weight, ch, px);
            let m = glyph.metrics;
            if m.width > 0 && m.height > 0 {
                let gx = pen.round() as i32 + m.xmin;
                let gy = baseline - m.ymin - m.height as i32;
                blend_coverage(pixmap, gx, gy, m.width, m.height, &glyph.coverage, color, clip);
            }
            pen += m.advance_width;
            prev = Some(ch);
        }
        width
    }
}

/// Splits on '\n', yielding (byte offset, paragraph).
fn split_paragraphs(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split('\n').map(move |p| {
        let start = offset;
        offset += p.len() + 1;
        (start, p)
    })
}

/// Byte ranges of space-separated words within `s`.
fn words(s: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut pos = 0;
    s.split(' ').filter_map(move |w| {
        let start = pos;
        pos += w.len() + 1;
        (!w.is_empty()).then_some((start, start + w.len()))
    })
}

/// Source-over blend of `color` × coverage onto a premultiplied RGBA pixmap.
#[allow(clippy::too_many_arguments)]
fn blend_coverage(
    pixmap: &mut Pixmap,
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    coverage: &[u8],
    color: Rgba,
    clip: Option<(i32, i32, i32, i32)>,
) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let (cx0, cy0, cx1, cy1) = clip.unwrap_or((0, 0, pw, ph));
    let (cx0, cy0, cx1, cy1) = (cx0.max(0), cy0.max(0), cx1.min(pw), cy1.min(ph));
    let data = pixmap.data_mut();
    let ca = color.a as u32;
    for gy in 0..h as i32 {
        let y = y0 + gy;
        if y < cy0 || y >= cy1 {
            continue;
        }
        for gx in 0..w as i32 {
            let x = x0 + gx;
            if x < cx0 || x >= cx1 {
                continue;
            }
            let cov = coverage[(gy as usize) * w + gx as usize] as u32;
            if cov == 0 {
                continue;
            }
            // Source alpha in 0..=255.
            let sa = div255(cov * ca);
            if sa == 0 {
                continue;
            }
            let i = ((y * pw + x) * 4) as usize;
            let inv = 255 - sa;
            data[i] = (div255(color.r as u32 * sa) + div255(data[i] as u32 * inv)) as u8;
            data[i + 1] = (div255(color.g as u32 * sa) + div255(data[i + 1] as u32 * inv)) as u8;
            data[i + 2] = (div255(color.b as u32 * sa) + div255(data[i + 2] as u32 * inv)) as u8;
            data[i + 3] = (sa + div255(data[i + 3] as u32 * inv)) as u8;
        }
    }
}

#[inline]
fn div255(v: u32) -> u32 {
    (v + 128 + ((v + 128) >> 8)) >> 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_long_text_and_keeps_words() {
        let mut t = TextRenderer::new();
        let text = "Vocal rest: 45 min voiced in the last hour";
        let mut lines = Vec::new();
        t.wrap(Weight::Regular, 20.0, 150.0, text, &mut lines);
        assert!(lines.len() >= 2);
        for &(s, e) in &lines {
            assert!(t.measure(Weight::Regular, 20.0, &text[s..e]) <= 150.0 || !text[s..e].contains(' '));
        }
        let joined: Vec<&str> = lines.iter().map(|&(s, e)| &text[s..e]).collect();
        assert_eq!(joined.join(" "), text);
    }

    #[test]
    fn fit_shrinks_until_it_fits() {
        let mut t = TextRenderer::new();
        let px = t.fit(Weight::SemiBold, 40.0, 10.0, 100.0, "A rather long label");
        assert!(px < 40.0);
        assert!(t.measure(Weight::SemiBold, px, "A rather long label") <= 100.0);
    }
}
