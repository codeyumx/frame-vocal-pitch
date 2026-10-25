//! Application icon (original artwork): a rising pitch contour next to a range bar with the
//! target band highlighted.

use tiny_skia::{Color, IntSize, PathBuilder, Pixmap};

use crate::draw::{self, Rgba, R};

fn render_pixmap(size_px: u32) -> Pixmap {
    let size_px = size_px.max(1);
    let mut pm = Pixmap::new(size_px, size_px).expect("non-zero icon size");
    pm.fill(Color::TRANSPARENT);
    // Artwork is designed on a 256-unit canvas.
    let s = size_px as f32 / 256.0;
    let r = |x: f32, y: f32, w: f32, h: f32| R::new(x * s, y * s, w * s, h * s);

    draw::fill_rounded(&mut pm, r(8.0, 8.0, 240.0, 240.0), 56.0 * s, Rgba::hex(0x1B2130, 255));

    // Range bar: track, green target band, white marker.
    draw::fill_rounded(&mut pm, r(170.0, 44.0, 36.0, 168.0), 18.0 * s, Rgba::hex(0x323B4F, 255));
    draw::fill_rounded(&mut pm, r(170.0, 82.0, 36.0, 74.0), 10.0 * s, Rgba::hex(0x2EBD59, 255));
    draw::fill_rounded(&mut pm, r(160.0, 104.0, 56.0, 14.0), 7.0 * s, Rgba::hex(0xFFFFFF, 255));

    // Pitch contour rising into the band.
    let mut pb = PathBuilder::new();
    pb.move_to(46.0 * s, 178.0 * s);
    pb.cubic_to(70.0 * s, 178.0 * s, 74.0 * s, 136.0 * s, 98.0 * s, 136.0 * s);
    pb.cubic_to(118.0 * s, 136.0 * s, 120.0 * s, 111.0 * s, 146.0 * s, 111.0 * s);
    if let Some(path) = pb.finish() {
        draw::stroke_path(&mut pm, &path, 16.0 * s, Rgba::hex(0xFFFFFF, 255));
    }
    pm
}

/// Icon as straight (non-premultiplied) RGBA8, `size_px` × `size_px`.
pub fn render_icon(size_px: u32) -> Vec<u8> {
    render_pixmap(size_px).take_demultiplied()
}

/// Icon encoded as PNG (for the `.desktop` hicolor icons and the dashboard thumbnail file).
pub fn icon_png(size_px: u32) -> Vec<u8> {
    render_pixmap(size_px)
        .encode_png()
        .expect("PNG encoding to memory cannot fail")
}

/// Encodes straight RGBA8 pixels as PNG.
pub(crate) fn encode_straight_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    // tiny-skia stores premultiplied pixels and demultiplies on encode, so premultiply first.
    let mut data = rgba.to_vec();
    for p in data.chunks_exact_mut(4) {
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(data, IntSize::from_wh(width, height).expect("non-zero size"))
        .expect("buffer matches size")
        .encode_png()
        .expect("PNG encoding to memory cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_sizes_and_transparent_corners() {
        for n in [48u32, 128, 256] {
            let px = render_icon(n);
            assert_eq!(px.len(), (n * n * 4) as usize);
            assert_eq!(px[3], 0, "corner is transparent");
            let c = ((n / 2 * n + n / 4) * 4) as usize;
            assert_eq!(px[c + 3], 255, "body is opaque");
        }
    }

    #[test]
    fn committed_icons_match_renderer() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");
        for n in [48u32, 128, 256] {
            let path = dir.join(format!("{n}.png"));
            let committed = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(
                committed == icon_png(n),
                "{} is stale; regenerate with `cargo run -p fvp-ui --example gen_icons`",
                path.display()
            );
        }
    }
}
