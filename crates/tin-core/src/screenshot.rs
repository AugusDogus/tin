//! A picture of the window a capture came from, kept as a PNG beside the capture's JSON.

/// An encoded PNG, before it's saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
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

impl Screenshot {
    /// Encodes `width × height` pixels, `stride` bytes a row, as a PNG.
    pub fn encode(
        pixels: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        order: PixelOrder,
    ) -> Result<Screenshot, String> {
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
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| format!("couldn't encode the screenshot: {e}"))?;
        writer.write_image_data(&rgba).map_err(|e| format!("couldn't encode the screenshot: {e}"))?;
        writer.finish().map_err(|e| format!("couldn't encode the screenshot: {e}"))?;
        Ok(Screenshot { png, width, height })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorders_channels_skips_row_padding_and_round_trips_through_png() {
        // 2×1 pixels, a stride of 12 (4 bytes of padding): red then green, as BGRX.
        let pixels = [0, 0, 255, 7, 0, 255, 0, 7, 9, 9, 9, 9];
        let shot = Screenshot::encode(&pixels, 2, 1, 12, PixelOrder::Bgrx).unwrap_or_else(|e| panic!("{e}"));
        let decoder = png::Decoder::new(std::io::Cursor::new(shot.png));
        let mut reader = decoder.read_info().unwrap_or_else(|e| panic!("{e}"));
        let mut out = vec![0; reader.output_buffer_size().unwrap_or(0)];
        reader.next_frame(&mut out).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out, [255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn a_short_buffer_is_an_error() {
        assert!(Screenshot::encode(&[0; 4], 2, 2, 8, PixelOrder::Rgba).is_err());
    }
}
