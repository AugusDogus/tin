//! Pictures of the screen: raw RGBA images, cropped to what the user picked, and saved as PNGs
//! beside the capture's JSON.

/// An encoded PNG, before it's saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Straight-alpha sRGB pixels, 4 bytes each, rows packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// How a compositor laid out the pixels it copied (Wayland `wl_shm` formats, which are
/// little-endian: `Argb8888` is B, G, R, A in memory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelOrder {
    /// B, G, R, A.
    Bgra,
    /// B, G, R, then a byte to ignore.
    Bgrx,
    /// R, G, B, A.
    Rgba,
    /// R, G, B, then a byte to ignore.
    Rgbx,
}

/// A rectangle in whole units (points or pixels; the caller says which).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// The rectangle between two corners, whichever order they're in.
    pub fn between(a: (i32, i32), b: (i32, i32)) -> Rect {
        Rect { x: a.0.min(b.0), y: a.1.min(b.1), width: (a.0 - b.0).abs(), height: (a.1 - b.1).abs() }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    pub fn area(&self) -> i64 {
        i64::from(self.width.max(0)) * i64::from(self.height.max(0))
    }

    /// The part inside `bounds`, if any.
    pub fn clamp_to(&self, bounds: Rect) -> Option<Rect> {
        let x0 = self.x.max(bounds.x);
        let y0 = self.y.max(bounds.y);
        let x1 = (self.x + self.width).min(bounds.x + bounds.width);
        let y1 = (self.y + self.height).min(bounds.y + bounds.height);
        (x1 > x0 && y1 > y0).then(|| Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
    }

    /// The same rectangle `factor` times larger (points to device pixels).
    pub fn scaled(&self, factor: f64) -> Rect {
        let s = |v: i32| (f64::from(v) * factor).round() as i32;
        Rect { x: s(self.x), y: s(self.y), width: s(self.width), height: s(self.height) }
    }
}

impl Image {
    /// Converts a compositor's buffer (`stride` bytes a row) to packed RGBA.
    pub fn from_shm(pixels: &[u8], width: u32, height: u32, stride: usize, order: PixelOrder) -> Result<Image, String> {
        let (w, h) = (width as usize, height as usize);
        if stride < w * 4 || pixels.len() < stride * h {
            return Err(format!("the copied image is {} bytes, too small for {width}×{height}", pixels.len()));
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for row in pixels.chunks(stride).take(h) {
            for p in row[..w * 4].as_chunks::<4>().0 {
                let [a, b, c, d] = *p;
                rgba.extend_from_slice(&match order {
                    PixelOrder::Bgra => [c, b, a, d],
                    PixelOrder::Bgrx => [c, b, a, 255],
                    PixelOrder::Rgba => [a, b, c, d],
                    PixelOrder::Rgbx => [a, b, c, 255],
                });
            }
        }
        Ok(Image { width, height, rgba })
    }

    /// The pixels inside `rect` (in this image's pixels), clamped to the image.
    pub fn crop(&self, rect: Rect) -> Option<Image> {
        let bounds = Rect { x: 0, y: 0, width: self.width as i32, height: self.height as i32 };
        let r = rect.clamp_to(bounds)?;
        let (x, w) = (r.x as usize * 4, r.width as usize * 4);
        let stride = self.width as usize * 4;
        let rgba = (r.y as usize..(r.y + r.height) as usize)
            .flat_map(|row| self.rgba[row * stride + x..row * stride + x + w].iter().copied())
            .collect();
        Some(Image { width: r.width as u32, height: r.height as u32, rgba })
    }

    pub fn png(&self) -> Result<Screenshot, String> {
        let fail = |e: png::EncodingError| format!("couldn't encode the screenshot: {e}");
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(fail)?;
        writer.write_image_data(&self.rgba).map_err(fail)?;
        writer.finish().map_err(fail)?;
        Ok(Screenshot { png, width: self.width, height: self.height })
    }
}

impl Screenshot {
    /// Encodes a compositor's buffer (`stride` bytes a row) as a PNG.
    pub fn encode(
        pixels: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        order: PixelOrder,
    ) -> Result<Screenshot, String> {
        Image::from_shm(pixels, width, height, stride, order)?.png()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png: Vec<u8>) -> Vec<u8> {
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let mut reader = decoder.read_info().unwrap_or_else(|e| panic!("{e}"));
        let mut out = vec![0; reader.output_buffer_size().unwrap_or(0)];
        reader.next_frame(&mut out).unwrap_or_else(|e| panic!("{e}"));
        out
    }

    #[test]
    fn reorders_channels_skips_row_padding_and_round_trips_through_png() {
        // 2×1 pixels, a stride of 12 (4 bytes of padding): red then green, as BGRX.
        let pixels = [0, 0, 255, 7, 0, 255, 0, 7, 9, 9, 9, 9];
        let shot = Screenshot::encode(&pixels, 2, 1, 12, PixelOrder::Bgrx).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(decode(shot.png), [255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn a_short_buffer_is_an_error() {
        assert!(Screenshot::encode(&[0; 4], 2, 2, 8, PixelOrder::Rgba).is_err());
    }

    #[test]
    fn crop_takes_the_inside_and_clamps_to_the_image() {
        // 3×2, each pixel's red channel is its index.
        let rgba: Vec<u8> = (0..6u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let image = Image { width: 3, height: 2, rgba };
        let crop = image.crop(Rect { x: 1, y: 0, width: 5, height: 9 }).unwrap_or_else(|| panic!("empty"));
        assert_eq!((crop.width, crop.height), (2, 2));
        assert_eq!(crop.rgba.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [1, 2, 4, 5]);
        assert_eq!(image.crop(Rect { x: 9, y: 9, width: 2, height: 2 }), None);
    }

    #[test]
    fn rects_from_corners_and_scaling() {
        assert_eq!(Rect::between((10, 40), (4, 20)), Rect { x: 4, y: 20, width: 6, height: 20 });
        assert_eq!(Rect { x: 1, y: 2, width: 3, height: 4 }.scaled(1.5), Rect { x: 2, y: 3, width: 5, height: 6 });
        assert!(Rect { x: 0, y: 0, width: 2, height: 2 }.contains(1, 1));
        assert!(!Rect { x: 0, y: 0, width: 2, height: 2 }.contains(2, 1));
    }
}
