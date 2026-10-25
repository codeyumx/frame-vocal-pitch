//! Small drawing helpers on top of tiny-skia (premultiplied RGBA8 pixmaps).

use tiny_skia::{
    Color, FillRule, LineCap, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

/// Straight (non-premultiplied) sRGB colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub(crate) const fn hex(rgb: u32, a: u8) -> Self {
        Self {
            r: (rgb >> 16) as u8,
            g: (rgb >> 8) as u8,
            b: rgb as u8,
            a,
        }
    }

    pub(crate) const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Linear interpolation in premultiplied space (so fading from a translucent colour does not
    /// flash the translucent colour's RGB at full strength), returned as straight colour.
    pub(crate) fn lerp(self, to: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let pa = |c: Self| {
            let a = c.a as f32 / 255.0;
            [c.r as f32 * a, c.g as f32 * a, c.b as f32 * a, c.a as f32]
        };
        let (f, g) = (pa(self), pa(to));
        let m: [f32; 4] = std::array::from_fn(|i| f[i] + (g[i] - f[i]) * t);
        let a = m[3];
        if a <= 0.0 {
            return Self::hex(0, 0);
        }
        let un = |v: f32| (v * 255.0 / a).round().clamp(0.0, 255.0) as u8;
        Self {
            r: un(m[0]),
            g: un(m[1]),
            b: un(m[2]),
            a: a.round().clamp(0.0, 255.0) as u8,
        }
    }

    fn color(self) -> Color {
        Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
}

pub(crate) fn paint(color: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color.color());
    p.anti_alias = true;
    p
}

/// Axis-aligned rectangle in pixel coordinates.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub(crate) const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub(crate) fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub(crate) fn right(&self) -> f32 {
        self.x + self.w
    }
    pub(crate) fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub(crate) fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub(crate) fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    pub(crate) fn inset(&self, d: f32) -> Self {
        Self::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }
    pub(crate) fn clip(&self) -> (i32, i32, i32, i32) {
        (
            self.x.floor() as i32,
            self.y.floor() as i32,
            self.right().ceil() as i32,
            self.bottom().ceil() as i32,
        )
    }
}

pub(crate) fn rounded_rect(r: R, radius: f32) -> Option<Path> {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    if rad <= 0.0 {
        return Rect::from_xywh(r.x, r.y, r.w, r.h).map(PathBuilder::from_rect);
    }
    // Cubic approximation of a quarter circle.
    let k = rad * 0.552_284_8;
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    let mut pb = PathBuilder::with_capacity(10, 30);
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - rad + k, y0, x1, y0 + rad - k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - rad + k, x1 - rad + k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + rad - k, y1, x0, y1 - rad + k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + rad - k, x0 + rad - k, y0, x0 + rad, y0);
    pb.close();
    pb.finish()
}

pub(crate) fn fill_rounded(pixmap: &mut Pixmap, r: R, radius: f32, color: Rgba) {
    if color.a == 0 {
        return;
    }
    if let Some(path) = rounded_rect(r, radius) {
        pixmap.fill_path(&path, &paint(color), FillRule::Winding, Transform::identity(), None);
    }
}

pub(crate) fn stroke_rounded(pixmap: &mut Pixmap, r: R, radius: f32, width: f32, color: Rgba) {
    if let Some(path) = rounded_rect(r, radius) {
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
    }
}

pub(crate) fn fill_circle(pixmap: &mut Pixmap, cx: f32, cy: f32, radius: f32, color: Rgba) {
    if let Some(path) = PathBuilder::from_circle(cx, cy, radius) {
        pixmap.fill_path(&path, &paint(color), FillRule::Winding, Transform::identity(), None);
    }
}

pub(crate) fn line(
    pixmap: &mut Pixmap,
    (x0, y0): (f32, f32),
    (x1, y1): (f32, f32),
    width: f32,
    color: Rgba,
) {
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    if let Some(path) = pb.finish() {
        stroke_path(pixmap, &path, width, color);
    }
}

pub(crate) fn stroke_path(pixmap: &mut Pixmap, path: &Path, width: f32, color: Rgba) {
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(path, &paint(color), &stroke, Transform::identity(), None);
}

/// Microphone glyph centred at (cx, cy) with total height `h`; `crossed` adds a slash whose
/// gap is painted in `gap` (the background colour) so the slash reads clearly.
pub(crate) fn mic_glyph(pixmap: &mut Pixmap, cx: f32, cy: f32, h: f32, fg: Rgba, gap: Rgba, crossed: bool) {
    let s = h / 100.0;
    let lw = 7.0 * s;
    // Capsule body.
    let body = R::new(cx - 15.0 * s, cy - 50.0 * s, 30.0 * s, 56.0 * s);
    fill_rounded(pixmap, body, 15.0 * s, fg);
    // Holder arc (U shape) around the lower half of the capsule.
    let mut pb = PathBuilder::new();
    let (ax0, ax1, ay) = (cx - 27.0 * s, cx + 27.0 * s, cy - 12.0 * s);
    pb.move_to(ax0, ay);
    pb.cubic_to(ax0, ay + 36.0 * s, ax1, ay + 36.0 * s, ax1, ay);
    if let Some(path) = pb.finish() {
        stroke_path(pixmap, &path, lw, fg);
    }
    // Stem and base.
    line(pixmap, (cx, cy + 15.0 * s), (cx, cy + 36.0 * s), lw, fg);
    line(pixmap, (cx - 18.0 * s, cy + 40.0 * s), (cx + 18.0 * s, cy + 40.0 * s), lw, fg);
    if crossed {
        let a = (cx - 38.0 * s, cy - 50.0 * s);
        let b = (cx + 38.0 * s, cy + 46.0 * s);
        line(pixmap, a, b, lw * 2.6, gap);
        line(pixmap, a, b, lw, fg);
    }
}

/// Exclamation mark glyph centred at (cx, cy) with total height `h`.
pub(crate) fn bang_glyph(pixmap: &mut Pixmap, cx: f32, cy: f32, h: f32, fg: Rgba) {
    let s = h / 100.0;
    let w = 16.0 * s;
    fill_rounded(pixmap, R::new(cx - w / 2.0, cy - 50.0 * s, w, 66.0 * s), w / 2.0, fg);
    fill_circle(pixmap, cx, cy + 40.0 * s, 10.0 * s, fg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_endpoints_and_alpha() {
        let a = Rgba::hex(0x2EBD59, 255);
        let b = Rgba::hex(0x202020, 153);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        let mid = a.lerp(b, 0.5);
        assert_eq!(mid.a, 204);
    }
}
