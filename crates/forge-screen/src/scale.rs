//! From the screen to the screenshot the model sees, and back.
//!
//! A large display is scaled down to fit [`MAX_SHOT_WIDTH`] ×
//! [`MAX_SHOT_HEIGHT`] (never up), keeping its aspect ratio. The model
//! names points in screenshot pixels; they are mapped back to screen pixels
//! before anything is clicked.

use crate::backend::{Frame, Point, Rect};

pub const MAX_SHOT_WIDTH: u32 = 1280;
pub const MAX_SHOT_HEIGHT: u32 = 800;

/// The mapping between one captured area and its screenshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scaling {
    pub area: Rect,
    pub shot_width: u32,
    pub shot_height: u32,
}

impl Scaling {
    /// Fit `area` within `max_width` × `max_height`.
    pub fn fit(area: Rect, max_width: u32, max_height: u32) -> Self {
        let width = area.width.max(1);
        let height = area.height.max(1);
        let scale = (f64::from(max_width) / f64::from(width))
            .min(f64::from(max_height) / f64::from(height))
            .min(1.0);
        Self {
            area,
            shot_width: ((f64::from(width) * scale).round() as u32).max(1),
            shot_height: ((f64::from(height) * scale).round() as u32).max(1),
        }
    }

    /// A screenshot point to the screen. `None` when it is outside the
    /// screenshot.
    pub fn to_screen(&self, shot: Point) -> Option<Point> {
        if shot.x < 0
            || shot.y < 0
            || shot.x >= self.shot_width as i32
            || shot.y >= self.shot_height as i32
        {
            return None;
        }
        let sx = f64::from(self.area.width) / f64::from(self.shot_width);
        let sy = f64::from(self.area.height) / f64::from(self.shot_height);
        let x = (f64::from(shot.x) * sx).floor() as i32;
        let y = (f64::from(shot.y) * sy).floor() as i32;
        Some(Point::new(
            self.area.x + x.min(self.area.width as i32 - 1),
            self.area.y + y.min(self.area.height as i32 - 1),
        ))
    }

    /// A screen point to the screenshot (for reporting where the pointer is).
    pub fn to_shot(&self, screen: Point) -> Point {
        let sx = f64::from(self.shot_width) / f64::from(self.area.width.max(1));
        let sy = f64::from(self.shot_height) / f64::from(self.area.height.max(1));
        Point::new(
            (f64::from(screen.x - self.area.x) * sx).round() as i32,
            (f64::from(screen.y - self.area.y) * sy).round() as i32,
        )
    }
}

/// Scale `frame` (which covers `scaling.area`) down to the screenshot size
/// by averaging the source pixels each target pixel covers.
pub fn downscale(frame: &Frame, scaling: &Scaling) -> Frame {
    let (tw, th) = (scaling.shot_width, scaling.shot_height);
    if tw == frame.width && th == frame.height {
        return frame.clone();
    }
    let (sw, sh) = (frame.width as u64, frame.height as u64);
    let mut rgba = vec![0u8; (tw * th * 4) as usize];
    for ty in 0..th as u64 {
        let y0 = ty * sh / th as u64;
        let y1 = (((ty + 1) * sh).div_ceil(th as u64)).clamp(y0 + 1, sh);
        for tx in 0..tw as u64 {
            let x0 = tx * sw / tw as u64;
            let x1 = (((tx + 1) * sw).div_ceil(tw as u64)).clamp(x0 + 1, sw);
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                let row = (y * sw * 4) as usize;
                for x in x0..x1 {
                    let at = row + (x * 4) as usize;
                    for (total, value) in sum.iter_mut().zip(&frame.rgba[at..at + 4]) {
                        *total += u64::from(*value);
                    }
                }
            }
            let count = (y1 - y0) * (x1 - x0);
            let out = ((ty * tw as u64 + tx) * 4) as usize;
            for (target, total) in rgba[out..out + 4].iter_mut().zip(sum) {
                *target = (total / count) as u8;
            }
        }
    }
    Frame {
        width: tw,
        height: th,
        rgba,
    }
}

/// Where a captured image keeps its colours: bytes per pixel, bytes per
/// row, and the byte offset of red, green and blue within a pixel. The
/// usual X11 formats (8 bits per channel in 24 or 32 bits) fit it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawLayout {
    pub bytes_per_pixel: usize,
    pub stride: usize,
    pub red: usize,
    pub green: usize,
    pub blue: usize,
}

/// Scale raw captured pixels (`width` × `height` in `layout`) straight to
/// the screenshot, averaging like [`downscale`], without first converting
/// the whole capture to RGBA.
pub fn downscale_raw(
    data: &[u8],
    layout: &RawLayout,
    width: u32,
    height: u32,
    scaling: &Scaling,
) -> Frame {
    let (tw, th) = (scaling.shot_width, scaling.shot_height);
    let (sw, sh) = (u64::from(width), u64::from(height));
    let bpp = layout.bytes_per_pixel;
    let mut rgba = vec![255u8; (tw * th * 4) as usize];
    for ty in 0..u64::from(th) {
        let y0 = ty * sh / u64::from(th);
        let y1 = ((ty + 1) * sh).div_ceil(u64::from(th)).clamp(y0 + 1, sh);
        for tx in 0..u64::from(tw) {
            let x0 = tx * sw / u64::from(tw);
            let x1 = ((tx + 1) * sw).div_ceil(u64::from(tw)).clamp(x0 + 1, sw);
            let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
            for y in y0..y1 {
                let row = &data[y as usize * layout.stride..];
                for x in x0..x1 {
                    let pixel = &row[x as usize * bpp..x as usize * bpp + bpp];
                    r += u64::from(pixel[layout.red]);
                    g += u64::from(pixel[layout.green]);
                    b += u64::from(pixel[layout.blue]);
                }
            }
            let count = (y1 - y0) * (x1 - x0);
            let out = ((ty * u64::from(tw) + tx) * 4) as usize;
            rgba[out] = (r / count) as u8;
            rgba[out + 1] = (g / count) as u8;
            rgba[out + 2] = (b / count) as u8;
        }
    }
    Frame {
        width: tw,
        height: th,
        rgba,
    }
}

/// Encode as PNG (RGBA, 8-bit).
pub fn encode_png(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder
            .write_header()
            .expect("writing to memory cannot fail");
        writer
            .write_image_data(&frame.rgba)
            .expect("the frame holds width × height RGBA pixels");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_large_display_is_scaled_down_keeping_its_shape() {
        let scaling = Scaling::fit(rect(0, 0, 2560, 1440), MAX_SHOT_WIDTH, MAX_SHOT_HEIGHT);
        assert_eq!((scaling.shot_width, scaling.shot_height), (1280, 720));
        let tall = Scaling::fit(rect(0, 0, 1600, 1600), MAX_SHOT_WIDTH, MAX_SHOT_HEIGHT);
        assert_eq!((tall.shot_width, tall.shot_height), (800, 800));
    }

    #[test]
    fn a_small_display_is_never_scaled_up() {
        let scaling = Scaling::fit(rect(0, 0, 1024, 768), MAX_SHOT_WIDTH, MAX_SHOT_HEIGHT);
        assert_eq!((scaling.shot_width, scaling.shot_height), (1024, 768));
        assert_eq!(
            scaling.to_screen(Point::new(10, 20)),
            Some(Point::new(10, 20))
        );
    }

    /// Points map back into the captured area, including its offset when
    /// the primary display is not at the origin.
    #[test]
    fn points_map_back_to_the_screen() {
        let scaling = Scaling::fit(rect(1920, 0, 2560, 1440), MAX_SHOT_WIDTH, MAX_SHOT_HEIGHT);
        assert_eq!(
            scaling.to_screen(Point::new(0, 0)),
            Some(Point::new(1920, 0))
        );
        assert_eq!(
            scaling.to_screen(Point::new(640, 360)),
            Some(Point::new(1920 + 1280, 720))
        );
        assert_eq!(
            scaling.to_screen(Point::new(1279, 719)),
            Some(Point::new(1920 + 2558, 1438))
        );
        assert_eq!(scaling.to_screen(Point::new(1280, 10)), None);
        assert_eq!(scaling.to_screen(Point::new(-1, 10)), None);
        assert_eq!(
            scaling.to_shot(Point::new(1920 + 1280, 720)),
            Point::new(640, 360)
        );
    }

    #[test]
    fn downscaling_averages_blocks() {
        // 4x2: left half white, right half black; halved to 2x1.
        let white = [255u8, 255, 255, 255];
        let black = [0u8, 0, 0, 255];
        let mut rgba = Vec::new();
        for _ in 0..2 {
            rgba.extend(white);
            rgba.extend(white);
            rgba.extend(black);
            rgba.extend(black);
        }
        let frame = Frame {
            width: 4,
            height: 2,
            rgba,
        };
        let scaling = Scaling::fit(rect(0, 0, 4, 2), 2, 1);
        let small = downscale(&frame, &scaling);
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.pixel(0, 0), white);
        assert_eq!(small.pixel(1, 0), black);
    }

    /// Pseudo-random bytes, so every pixel differs.
    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    /// The direct path gives exactly what converting every pixel and then
    /// downscaling gives, for 32- and 24-bit layouts, at any ratio.
    #[test]
    fn scaling_raw_pixels_matches_converting_then_scaling() {
        let layouts = [
            // BGRX, as X servers send 24-bit colour in 32 bits.
            (4, 2, 1, 0),
            // Packed 24-bit RGB.
            (3, 0, 1, 2),
        ];
        for (bytes_per_pixel, red, green, blue) in layouts {
            for (width, height, max_w, max_h) in [
                (64u32, 48u32, 32u32, 24u32),
                (37, 23, 16, 10),
                (300, 200, 1280, 800),
                (2560, 16, 1280, 800),
            ] {
                // Rows padded to 4 bytes, as X pads scanlines.
                let stride = (width as usize * bytes_per_pixel).div_ceil(4) * 4;
                let data = noise(stride * height as usize, width * 31 + height);
                let layout = RawLayout {
                    bytes_per_pixel,
                    stride,
                    red,
                    green,
                    blue,
                };
                let mut rgba = Vec::new();
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let at = y * stride + x * bytes_per_pixel;
                        rgba.extend([data[at + red], data[at + green], data[at + blue], 255]);
                    }
                }
                let frame = Frame {
                    width,
                    height,
                    rgba,
                };
                let scaling = Scaling::fit(rect(0, 0, width, height), max_w, max_h);
                assert_eq!(
                    downscale_raw(&data, &layout, width, height, &scaling),
                    downscale(&frame, &scaling),
                    "{bytes_per_pixel} bytes per pixel, {width}x{height} into {max_w}x{max_h}"
                );
            }
        }
    }

    #[test]
    fn png_encoding_round_trips_the_size() {
        let frame = Frame {
            width: 3,
            height: 2,
            rgba: vec![200; 3 * 2 * 4],
        };
        let png = encode_png(&frame);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let meta = forge_types::inspect_image(&png).unwrap();
        assert_eq!((meta.width, meta.height), (Some(3), Some(2)));
    }
}
