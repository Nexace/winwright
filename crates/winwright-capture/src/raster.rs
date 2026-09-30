//! Pure pixel math: size bounds, row-pitch copies, alpha forcing, and the region crop/compose
//! plan. No Windows calls live here, so every rule is unit-tested.
//!
//! Coordinates come in two kinds only. [`PhysicalRect`]/[`PhysicalPoint`] are signed physical
//! virtual-desktop pixels; [`LocalRect`] is capture-local pixels relative to one capture's
//! top-left. Windows.Graphics.Capture frames are already physical pixels, so converting between
//! the two is a pure translation (subtract the capture origin) and never a scale (spec §18/§43).

use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::{WinwrightError, WinwrightResult};

pub const BYTES_PER_PIXEL: usize = 4;
/// Largest accepted edge; also the D3D11 texture limit.
pub const MAX_EDGE: u32 = 16_384;
/// Largest accepted raw BGRA buffer.
pub const MAX_RAW_BYTES: u64 = 256 * 1024 * 1024;

fn too_large(width: u32, height: u32) -> WinwrightError {
    WinwrightError::CaptureFailed {
        reason: format!(
            "{width}x{height} exceeds the capture limit ({MAX_EDGE}x{MAX_EDGE}, {} MB raw)",
            MAX_RAW_BYTES / (1024 * 1024)
        ),
    }
}

/// Validates dimensions against the capture bounds and returns the raw BGRA byte length.
pub fn raw_len(width: u32, height: u32) -> WinwrightResult<usize> {
    if width == 0 || height == 0 {
        return Err(WinwrightError::CaptureFailed {
            reason: format!("the capture target is empty ({width}x{height})"),
        });
    }
    if width > MAX_EDGE || height > MAX_EDGE {
        return Err(too_large(width, height));
    }
    let bytes = u64::from(width) * u64::from(height) * BYTES_PER_PIXEL as u64;
    if bytes > MAX_RAW_BYTES {
        return Err(too_large(width, height));
    }
    Ok(bytes as usize)
}

/// Rejects empty or oversized regions and returns their pixel size.
pub fn region_size(rect: &PhysicalRect) -> WinwrightResult<(u32, u32)> {
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    if width <= 0 || height <= 0 {
        return Err(WinwrightError::invalid(format!(
            "capture region [{}, {}, {}, {}] is empty",
            rect.left, rect.top, rect.right, rect.bottom
        )));
    }
    let clamp = |v: i64| u32::try_from(v).unwrap_or(u32::MAX);
    let (width, height) = (clamp(width), clamp(height));
    raw_len(width, height)?;
    Ok((width, height))
}

/// Tightly packed BGRA8 pixels (stride = `width * 4`).
#[derive(Clone, PartialEq, Eq)]
pub struct Bgra {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Bgra {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Sizes only: pixel data never goes to logs.
        f.debug_struct("Bgra")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

impl Bgra {
    /// Opaque black canvas; the fill for gaps between monitors.
    pub fn black(width: u32, height: u32) -> WinwrightResult<Self> {
        let mut pixels = vec![0; raw_len(width, height)?];
        force_opaque(&mut pixels);
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn stride(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL
    }
}

/// Copies `height` rows of `width` BGRA pixels out of a mapped buffer whose rows start
/// `row_pitch` bytes apart (GPU rows are padded, so `row_pitch >= width * 4`).
///
/// Panics if `src` is shorter than `row_pitch * (height - 1) + width * 4`.
pub fn copy_rows(src: &[u8], row_pitch: usize, width: u32, height: u32) -> Vec<u8> {
    let row = width as usize * BYTES_PER_PIXEL;
    assert!(row_pitch >= row, "row pitch {row_pitch} < row size {row}");
    let mut out = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        let start = y * row_pitch;
        out.extend_from_slice(&src[start..start + row]);
    }
    out
}

/// Sets every alpha byte to 255. Capture surfaces are premultiplied, so this equals compositing
/// over black, matching the fill used for gaps between monitors.
pub fn force_opaque(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<BYTES_PER_PIXEL>().0 {
        px[3] = 255;
    }
}

/// Pixel rectangle inside one capture, relative to that capture's top-left pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One monitor's contribution to a region capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    /// Index into the `sources` given to [`plan_region`].
    pub source: usize,
    /// Pixels to crop out of that source's capture.
    pub crop: LocalRect,
    /// Where the crop lands in the output canvas.
    pub dst_x: u32,
    pub dst_y: u32,
}

pub const fn top_left(rect: &PhysicalRect) -> PhysicalPoint {
    PhysicalPoint {
        x: rect.left,
        y: rect.top,
    }
}

pub fn intersect(a: &PhysicalRect, b: &PhysicalRect) -> Option<PhysicalRect> {
    let r = PhysicalRect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    );
    (!r.is_empty()).then_some(r)
}

/// Translates a physical desktop rectangle into the capture-local pixels of a capture whose
/// top-left pixel sits at `origin`: subtract the origin, never scale. `None` when the rectangle
/// is empty or starts above/left of the capture.
pub fn to_local(rect: &PhysicalRect, origin: PhysicalPoint) -> Option<LocalRect> {
    let x = i64::from(rect.left) - i64::from(origin.x);
    let y = i64::from(rect.top) - i64::from(origin.y);
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(LocalRect {
        x: u32::try_from(x).ok()?,
        y: u32::try_from(y).ok()?,
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
    })
}

/// Splits `region` over the monitors (`sources`, physical bounds) it intersects. Parts of the
/// region no monitor covers get no tile and stay black in the composed image.
pub fn plan_region(region: &PhysicalRect, sources: &[PhysicalRect]) -> Vec<Tile> {
    sources
        .iter()
        .enumerate()
        .filter_map(|(source, bounds)| {
            let part = intersect(region, bounds)?;
            let crop = to_local(&part, top_left(bounds))?;
            let dst = to_local(&part, top_left(region))?;
            Some(Tile {
                source,
                crop,
                dst_x: dst.x,
                dst_y: dst.y,
            })
        })
        .collect()
}

/// Clamps a crop to the pixels a frame actually delivered (a frame can be smaller than the
/// monitor bounds mid mode-change). `None` when nothing of the crop is left.
pub fn clamp_to_frame(crop: LocalRect, frame_width: u32, frame_height: u32) -> Option<LocalRect> {
    if crop.width == 0 || crop.height == 0 || crop.x >= frame_width || crop.y >= frame_height {
        return None;
    }
    Some(LocalRect {
        x: crop.x,
        y: crop.y,
        width: crop.width.min(frame_width - crop.x),
        height: crop.height.min(frame_height - crop.y),
    })
}

/// Copies `src` into `canvas` with its top-left at (`dst_x`, `dst_y`), clipped to the canvas.
pub fn blit(canvas: &mut Bgra, src: &Bgra, dst_x: u32, dst_y: u32) {
    if dst_x >= canvas.width || dst_y >= canvas.height {
        return;
    }
    let row = src.width.min(canvas.width - dst_x) as usize * BYTES_PER_PIXEL;
    let rows = src.height.min(canvas.height - dst_y) as usize;
    let (canvas_stride, src_stride) = (canvas.stride(), src.stride());
    for y in 0..rows {
        let d = (dst_y as usize + y) * canvas_stride + dst_x as usize * BYTES_PER_PIXEL;
        let s = y * src_stride;
        canvas.pixels[d..d + row].copy_from_slice(&src.pixels[s..s + row]);
    }
}

/// Smallest rectangle covering every monitor: the virtual desktop.
pub fn union(rects: &[PhysicalRect]) -> Option<PhysicalRect> {
    rects
        .iter()
        .copied()
        .filter(|r| !r.is_empty())
        .reduce(|a, b| {
            PhysicalRect::new(
                a.left.min(b.left),
                a.top.min(b.top),
                a.right.max(b.right),
                a.bottom.max(b.bottom),
            )
        })
}

/// WIC `ImageQuality` for JPEG: quality clamped to 1..=100, mapped onto 0.01..=1.0.
pub fn jpeg_quality(quality: u8) -> f32 {
    f32::from(quality.clamp(1, 100)) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> PhysicalRect {
        PhysicalRect::new(l, t, r, b)
    }

    fn local(x: u32, y: u32, width: u32, height: u32) -> LocalRect {
        LocalRect {
            x,
            y,
            width,
            height,
        }
    }

    /// A 1-pixel-per-value image whose blue byte encodes its column and green byte its row.
    fn gradient(width: u32, height: u32) -> Bgra {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[x as u8, y as u8, 7, 255]);
            }
        }
        Bgra {
            width,
            height,
            pixels,
        }
    }

    fn px(img: &Bgra, x: u32, y: u32) -> [u8; 4] {
        let i = y as usize * img.stride() + x as usize * 4;
        img.pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn size_bounds() {
        assert_eq!(raw_len(1920, 1080).unwrap(), 1920 * 1080 * 4);
        assert_eq!(raw_len(8192, 8192).unwrap(), 256 * 1024 * 1024);
        for (w, h) in [(0, 10), (10, 0), (16_385, 1), (1, 16_385), (8192, 8193)] {
            let err = raw_len(w, h).unwrap_err();
            assert_eq!(err.code().as_str(), "CAPTURE_FAILED", "{w}x{h}");
        }
    }

    #[test]
    fn region_size_rejects_empty_and_huge() {
        assert_eq!(region_size(&rect(-10, -10, 10, 20)).unwrap(), (20, 30));
        let empty = region_size(&rect(5, 5, 5, 50)).unwrap_err();
        assert_eq!(empty.code().as_str(), "INVALID_REQUEST");
        let inverted = region_size(&rect(50, 5, 5, 50)).unwrap_err();
        assert_eq!(inverted.code().as_str(), "INVALID_REQUEST");
        let huge = region_size(&rect(i32::MIN, 0, i32::MAX, 10)).unwrap_err();
        assert_eq!(huge.code().as_str(), "CAPTURE_FAILED");
    }

    #[test]
    fn copy_rows_skips_row_padding() {
        // 2x3 image, each row padded from 8 to 12 bytes with 0xEE.
        let mut src = Vec::new();
        for y in 0..3u8 {
            src.extend_from_slice(&[y, 0, 0, 1, y, 1, 0, 1]);
            if y < 2 {
                src.extend_from_slice(&[0xEE; 4]);
            }
        }
        let out = copy_rows(&src, 12, 2, 3);
        assert_eq!(out.len(), 2 * 3 * 4);
        assert!(!out.contains(&0xEE));
        assert_eq!(&out[16..24], &[2, 0, 0, 1, 2, 1, 0, 1]);
        // Unpadded input is copied verbatim.
        assert_eq!(copy_rows(&out, 8, 2, 3), out);
    }

    #[test]
    fn force_opaque_touches_alpha_only() {
        let mut px = vec![1, 2, 3, 0, 4, 5, 6, 128];
        force_opaque(&mut px);
        assert_eq!(px, [1, 2, 3, 255, 4, 5, 6, 255]);
        let canvas = Bgra::black(2, 1).unwrap();
        assert_eq!(canvas.pixels, [0, 0, 0, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn to_local_subtracts_origin_without_scaling() {
        // Monitor left of the primary with a negative origin.
        let origin = PhysicalPoint { x: -1920, y: -200 };
        assert_eq!(
            to_local(&rect(-1900, -100, -1800, 0), origin),
            Some(local(20, 100, 100, 100))
        );
        assert_eq!(to_local(&rect(-1930, -100, -1800, 0), origin), None);
        assert_eq!(to_local(&rect(0, 0, 0, 10), origin), None);
    }

    #[test]
    fn region_inside_one_monitor() {
        let monitors = [rect(0, 0, 2560, 1440), rect(2560, 0, 4480, 1080)];
        let tiles = plan_region(&rect(100, 200, 300, 250), &monitors);
        assert_eq!(
            tiles,
            [Tile {
                source: 0,
                crop: local(100, 200, 200, 50),
                dst_x: 0,
                dst_y: 0
            }]
        );
    }

    #[test]
    fn region_spanning_two_monitors_with_negative_origin() {
        // Secondary monitor at 150% sits left of and above the primary.
        let monitors = [rect(0, 0, 1920, 1080), rect(-2880, -300, 0, 1320)];
        let tiles = plan_region(&rect(-100, 1000, 100, 1100), &monitors);
        assert_eq!(
            tiles,
            [
                Tile {
                    source: 0,
                    crop: local(0, 1000, 100, 80),
                    dst_x: 100,
                    dst_y: 0
                },
                Tile {
                    source: 1,
                    crop: local(2780, 1300, 100, 100),
                    dst_x: 0,
                    dst_y: 0
                },
            ]
        );
    }

    #[test]
    fn gaps_and_offscreen_regions_get_no_tiles() {
        // Two monitors of different heights leave a gap under the shorter one.
        let monitors = [rect(0, 0, 1000, 800), rect(1000, 0, 2000, 600)];
        let tiles = plan_region(&rect(900, 500, 1100, 700), &monitors);
        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0].crop, local(900, 500, 100, 200));
        assert_eq!((tiles[0].dst_x, tiles[0].dst_y), (0, 0));
        assert_eq!(tiles[1].crop, local(0, 500, 100, 100));
        assert_eq!((tiles[1].dst_x, tiles[1].dst_y), (100, 0));
        assert!(plan_region(&rect(5000, 5000, 5100, 5100), &monitors).is_empty());
    }

    #[test]
    fn compose_fills_gaps_black() {
        let monitors = [rect(0, 0, 4, 4), rect(4, 0, 8, 2)];
        let region = rect(2, 1, 6, 3); // 4x2, bottom-right pixel pair is in the gap.
        let (w, h) = region_size(&region).unwrap();
        let mut canvas = Bgra::black(w, h).unwrap();
        let captures = [gradient(4, 4), gradient(4, 2)];
        for tile in plan_region(&region, &monitors) {
            let src = &captures[tile.source];
            let crop = tile.crop;
            let mut part = Bgra {
                width: crop.width,
                height: crop.height,
                pixels: Vec::new(),
            };
            for y in crop.y..crop.y + crop.height {
                let s = y as usize * src.stride() + crop.x as usize * 4;
                part.pixels
                    .extend_from_slice(&src.pixels[s..s + crop.width as usize * 4]);
            }
            blit(&mut canvas, &part, tile.dst_x, tile.dst_y);
        }
        // Monitor 0 contributes desktop (2..4, 1..3) = its local (2..4, 1..3).
        assert_eq!(px(&canvas, 0, 0), [2, 1, 7, 255]);
        assert_eq!(px(&canvas, 1, 1), [3, 2, 7, 255]);
        // Monitor 1 contributes desktop (4..6, 1..2) = its local (0..2, 1..2).
        assert_eq!(px(&canvas, 2, 0), [0, 1, 7, 255]);
        assert_eq!(px(&canvas, 3, 0), [1, 1, 7, 255]);
        // Desktop (4..6, 2) is below monitor 1: black gap.
        assert_eq!(px(&canvas, 2, 1), [0, 0, 0, 255]);
        assert_eq!(px(&canvas, 3, 1), [0, 0, 0, 255]);
    }

    #[test]
    fn blit_clips_to_canvas() {
        let mut canvas = Bgra::black(3, 3).unwrap();
        blit(&mut canvas, &gradient(4, 4), 2, 1);
        assert_eq!(px(&canvas, 2, 1), [0, 0, 7, 255]);
        assert_eq!(px(&canvas, 2, 2), [0, 1, 7, 255]);
        assert_eq!(px(&canvas, 1, 1), [0, 0, 0, 255]);
        blit(&mut canvas, &gradient(1, 1), 3, 0); // fully outside: no-op, no panic
    }

    #[test]
    fn clamp_to_frame_trims_to_delivered_pixels() {
        let crop = local(1900, 1000, 100, 100);
        assert_eq!(
            clamp_to_frame(crop, 1920, 1080),
            Some(local(1900, 1000, 20, 80))
        );
        assert_eq!(clamp_to_frame(crop, 1900, 1080), None);
        assert_eq!(
            clamp_to_frame(local(0, 0, 10, 10), 1920, 1080),
            Some(local(0, 0, 10, 10))
        );
    }

    #[test]
    fn union_covers_virtual_desktop() {
        let monitors = [rect(0, 0, 1920, 1080), rect(-2880, -300, 0, 1320)];
        assert_eq!(union(&monitors), Some(rect(-2880, -300, 1920, 1320)));
        assert_eq!(union(&[]), None);
    }

    #[test]
    fn jpeg_quality_is_clamped() {
        assert_eq!(jpeg_quality(0), 0.01);
        assert_eq!(jpeg_quality(85), 0.85);
        assert_eq!(jpeg_quality(100), 1.0);
        assert_eq!(jpeg_quality(255), 1.0);
    }
}
