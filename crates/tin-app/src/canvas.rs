//! Software drawing into a Wayland `Argb8888` buffer (B, G, R, A bytes) for the picker's
//! frozen-screen surfaces: the dimmed frame, the bright highlight, its outline and labels.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use tin_core::screenshot::{Image, Rect};

/// A colour as straight-alpha sRGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

/// A frozen screen at buffer resolution, bright and dimmed, ready to compose frames from.
pub struct Backdrop {
    pub width: u32,
    pub height: u32,
    bright: Vec<u8>,
    dim: Vec<u8>,
}

/// How much of the screen's brightness the dimmed frame keeps.
const DIM_KEEP: u16 = 115;

impl Backdrop {
    /// `image` scaled (nearest pixel) to `width × height`, in buffer byte order.
    pub fn new(image: &Image, width: u32, height: u32) -> Backdrop {
        let (sw, sh) = (image.width.max(1) as usize, image.height.max(1) as usize);
        let mut bright = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height as usize {
            let sy = (y * sh / height.max(1) as usize).min(sh - 1);
            for x in 0..width as usize {
                let sx = (x * sw / width.max(1) as usize).min(sw - 1);
                let i = (sy * sw + sx) * 4;
                let p = image.rgba.get(i..i + 4).unwrap_or(&[0, 0, 0, 255]);
                bright.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        let dim = bright
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|[b, g, r, a]| [darken(*b), darken(*g), darken(*r), *a])
            .collect();
        Backdrop { width, height, bright, dim }
    }

    /// Writes the dimmed frame into `out`, with `bright` (buffer pixels) undimmed.
    pub fn compose(&self, out: &mut [u8], bright: Option<Rect>) {
        let n = self.dim.len().min(out.len());
        out[..n].copy_from_slice(&self.dim[..n]);
        let bounds = Rect { x: 0, y: 0, width: self.width as i32, height: self.height as i32 };
        if let Some(r) = bright.and_then(|r| r.clamp_to(bounds)) {
            let stride = self.width as usize * 4;
            for y in r.y as usize..(r.y + r.height) as usize {
                let (a, b) = (y * stride + r.x as usize * 4, y * stride + (r.x + r.width) as usize * 4);
                if b <= out.len() {
                    out[a..b].copy_from_slice(&self.bright[a..b]);
                }
            }
        }
    }
}

fn darken(c: u8) -> u8 {
    ((u16::from(c) * DIM_KEEP) / 255) as u8
}

/// Drawing on a composed frame.
pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub width: u32,
    pub height: u32,
}

impl Canvas<'_> {
    /// Blends a rectangle of `color` over the frame.
    pub fn fill(&mut self, rect: Rect, color: Rgba) {
        let bounds = Rect { x: 0, y: 0, width: self.width as i32, height: self.height as i32 };
        let Some(r) = rect.clamp_to(bounds) else { return };
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                self.blend(x, y, color, 255);
            }
        }
    }

    /// A `thickness`-wide outline just inside `rect`.
    pub fn outline(&mut self, rect: Rect, thickness: i32, color: Rgba) {
        let Rect { x, y, width: w, height: h } = rect;
        let t = thickness.min(w / 2).min(h / 2).max(1);
        self.fill(Rect { x, y, width: w, height: t }, color);
        self.fill(Rect { x, y: y + h - t, width: w, height: t }, color);
        self.fill(Rect { x, y: y + t, width: t, height: h - 2 * t }, color);
        self.fill(Rect { x: x + w - t, y: y + t, width: t, height: h - 2 * t }, color);
    }

    /// A dark pill with `text` in it, its top-left at `(x, y)`; returns its width. Nothing is
    /// drawn without a font.
    pub fn pill(&mut self, font: Option<&FontVec>, x: i32, y: i32, text: &str, size: f32, max_width: i32) -> i32 {
        let Some(font) = font else { return 0 };
        let pad = (size * 0.7) as i32;
        let height = (size * 1.9) as i32;
        let width = (measure(font, text, size).ceil() as i32 + 2 * pad + 4).min(max_width);
        self.fill(Rect { x, y, width, height }, Rgba(0, 0, 0, 200));
        let baseline = y as f32 + (height as f32 + size * 0.7) / 2.0;
        self.text(font, (x + pad, baseline), text, size, x + width - pad, Rgba(255, 255, 255, 255));
        width
    }

    /// `text` starting at `(x, baseline)`, clipped at `right`.
    pub fn text(&mut self, font: &FontVec, (x, baseline): (i32, f32), text: &str, size: f32, right: i32, color: Rgba) {
        let scaled = font.as_scaled(PxScale::from(size));
        let mut pen = x as f32;
        let mut last = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(prev) = last {
                pen += scaled.kern(prev, id);
            }
            let advance = scaled.h_advance(id);
            if pen + advance > right as f32 {
                break;
            }
            let glyph = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(pen, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let b = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let alpha = (coverage.clamp(0.0, 1.0) * 255.0) as u8;
                    self.blend(b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, color, alpha);
                });
            }
            pen += advance;
            last = Some(id);
        }
    }

    fn blend(&mut self, x: i32, y: i32, Rgba(r, g, b, a): Rgba, coverage: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let Some(px) = self.buf.get_mut(i..i + 3) else { return };
        let alpha = u16::from(a) * u16::from(coverage) / 255;
        for (dst, src) in px.iter_mut().zip([b, g, r]) {
            *dst = ((u16::from(src) * alpha + u16::from(*dst) * (255 - alpha)) / 255) as u8;
        }
    }
}

pub fn measure(font: &FontVec, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    text.chars().map(|c| scaled.h_advance(scaled.glyph_id(c))).sum()
}

/// The desktop's sans-serif font, through fontconfig. `None` leaves labels out.
pub fn system_font() -> Option<FontVec> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}", "sans-serif:weight=medium"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines();
    let (path, index) = (lines.next()?, lines.next().and_then(|i| i.parse().ok()).unwrap_or(0));
    FontVec::try_from_vec_and_index(std::fs::read(path).ok()?, index).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Image {
        // 2×1: red, blue.
        Image { width: 2, height: 1, rgba: vec![200, 0, 0, 255, 0, 0, 200, 255] }
    }

    #[test]
    fn backdrop_converts_to_buffer_order_scales_and_dims() {
        let backdrop = Backdrop::new(&image(), 4, 1);
        let mut out = vec![0; 16];
        backdrop.compose(&mut out, None);
        // Dimmed red in B, G, R, A order, twice (scaled up), then dimmed blue.
        assert_eq!(&out[0..4], &[0, 0, darken(200), 255]);
        assert_eq!(&out[4..8], &out[0..4]);
        assert_eq!(&out[8..12], &[darken(200), 0, 0, 255]);
    }

    #[test]
    fn the_highlight_stays_bright() {
        let backdrop = Backdrop::new(&image(), 2, 1);
        let mut out = vec![0; 8];
        backdrop.compose(&mut out, Some(Rect { x: 1, y: 0, width: 5, height: 5 }));
        assert_eq!(&out[0..4], &[0, 0, darken(200), 255]);
        assert_eq!(&out[4..8], &[200, 0, 0, 255]);
    }

    #[test]
    fn fill_blends_and_clips() {
        let mut buf = vec![0u8; 2 * 2 * 4];
        let mut canvas = Canvas { buf: &mut buf, width: 2, height: 2 };
        canvas.fill(Rect { x: 1, y: 1, width: 9, height: 9 }, Rgba(255, 255, 255, 255));
        assert_eq!(&buf[12..16], &[255, 255, 255, 0]);
        assert_eq!(&buf[0..4], &[0, 0, 0, 0]);
    }
}
