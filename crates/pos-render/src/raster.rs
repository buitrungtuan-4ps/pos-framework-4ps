// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The scanline rasteriser: glyph outlines in, one bit per pixel out.
//!
//! # Why this is integer arithmetic
//!
//! A receipt printed in Hanoi and the same receipt printed in Ho Chi Minh City must be the same
//! bytes. Floating point does not promise that: the same expression can round differently across
//! targets and optimisation levels, and a rounding difference at a coverage threshold flips a pixel,
//! which changes the raster, which changes the bytes on the wire. So every coordinate here is a
//! fixed-point integer in units of 1/[`SUBPIXEL`] of a pixel, every curve is flattened by exact
//! integer arithmetic, and coverage is counted rather than measured. No float is named in this
//! module at all: the crate's `clippy.toml` lifts the workspace ban on the *type* only for the font
//! parser's outline callback in [`crate::text`], and `clippy::float_arithmetic` stays denied
//! everywhere, so no float can be computed with anywhere in this crate.
//!
//! # The algorithm
//!
//! An outline is a set of closed contours. Filling one is: for each of [`SAMPLES_PER_ROW`] sample
//! lines across a pixel row, find where the contours cross that line, sort the crossings, and walk
//! them accumulating a winding number — the interior is where the winding is non-zero, which is the
//! rule TrueType outlines are drawn under. Horizontal coverage inside a span is exact (a span
//! contributes its overlap with each pixel, in 1/64ths), vertical coverage is sampled. A pixel is
//! printed when it is at least half covered, because a thermal head has no grey.

use core::num::NonZeroU16;

/// Fixed-point precision: coordinates are in 1/64 of a pixel, as TrueType hinting uses.
pub(crate) const SUBPIXEL: i32 = 64;

/// Sample lines per pixel row. Four is the point where more stops changing the thresholded output
/// for text at receipt sizes, and each one costs a pass over the edge list.
const SAMPLES_PER_ROW: i32 = 4;

/// Coverage of a fully-inked pixel: every sample line, fully spanned.
const FULL_COVERAGE: u32 = (SAMPLES_PER_ROW * SUBPIXEL) as u32;

/// A point in 1/[`SUBPIXEL`] pixel units, y downwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Point {
    /// Horizontal position, in 1/[`SUBPIXEL`] pixels.
    pub x: i32,
    /// Vertical position, in 1/[`SUBPIXEL`] pixels, increasing downwards.
    pub y: i32,
}

impl Point {
    /// A point at these subpixel coordinates.
    pub(crate) const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// One straight edge of a flattened contour.
#[derive(Debug, Clone, Copy)]
struct Edge {
    /// Upper end (smaller y).
    top: Point,
    /// Lower end (larger y).
    bottom: Point,
    /// `1` when the contour ran downwards through this edge, `-1` when upwards. This is what makes
    /// the winding rule work, and dropping it would fill the counters of an "o".
    direction: i32,
}

/// A monochrome image, one bit per pixel, rows padded to whole bytes — the shape
/// [`pos_ports::printer::PrintBlock::Bitmap`] carries and ESC/POS `GS v 0` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    width: NonZeroU16,
    height: u16,
    bits: Vec<u8>,
}

impl Bitmap {
    /// Pixels per row.
    #[must_use]
    pub const fn width(&self) -> NonZeroU16 {
        self.width
    }

    /// Rows.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// The bits, row-major, `width.div_ceil(8)` bytes per row, most significant bit leftmost, a set
    /// bit meaning ink.
    #[must_use]
    pub fn bits(&self) -> &[u8] {
        &self.bits
    }

    /// Whether nothing is inked — a line that shaped to no visible glyphs.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.bits.iter().all(|byte| *byte == 0)
    }

    /// This bitmap as a print block.
    #[must_use]
    pub fn into_block(self) -> pos_ports::printer::PrintBlock {
        pos_ports::printer::PrintBlock::Bitmap {
            width: self.width,
            bits: self.bits,
        }
    }

    /// Whether the pixel at `(x, y)` is inked. Out-of-range coordinates read as blank, which is what
    /// they are.
    #[must_use]
    pub fn pixel(&self, x: u16, y: u16) -> bool {
        if x >= self.width.get() || y >= self.height {
            return false;
        }
        let stride = usize::from(self.width.get()).div_ceil(8);
        let index = usize::from(y) * stride + usize::from(x) / 8;
        let mask = 0x80_u8 >> (usize::from(x) % 8);
        self.bits.get(index).is_some_and(|byte| byte & mask != 0)
    }
}

/// Collects edges, then fills them.
#[derive(Debug)]
pub(crate) struct Canvas {
    width: usize,
    height: usize,
    edges: Vec<Edge>,
}

impl Canvas {
    /// An empty canvas `width` × `height` pixels.
    pub(crate) fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            edges: Vec::new(),
        }
    }

    /// Adds a straight edge. Horizontal edges are dropped: they cross no sample line, and keeping
    /// them would only add work.
    pub(crate) fn line(&mut self, from: Point, to: Point) {
        if from.y == to.y {
            return;
        }
        let (top, bottom, direction) = if from.y < to.y {
            (from, to, 1)
        } else {
            (to, from, -1)
        };
        self.edges.push(Edge {
            top,
            bottom,
            direction,
        });
    }

    /// Adds a quadratic Bézier, flattened into straight edges.
    ///
    /// Each point is evaluated in exact integer arithmetic — `((n-i)²·p₀ + 2(n-i)i·p₁ + i²·p₂) / n²`
    /// — so the same curve flattens to the same edges on every target.
    pub(crate) fn quad(&mut self, from: Point, control: Point, to: Point) {
        let steps = segments(&[from, control, to]);
        let mut previous = from;
        for step in 1..=steps {
            let i = i64::from(step);
            let n = i64::from(steps);
            let j = n - i;
            let point = Point::new(
                div_round(
                    j * j * i64::from(from.x)
                        + 2 * j * i * i64::from(control.x)
                        + i * i * i64::from(to.x),
                    n * n,
                ),
                div_round(
                    j * j * i64::from(from.y)
                        + 2 * j * i * i64::from(control.y)
                        + i * i * i64::from(to.y),
                    n * n,
                ),
            );
            self.line(previous, point);
            previous = point;
        }
    }

    /// Adds a cubic Bézier, flattened into straight edges.
    pub(crate) fn cubic(&mut self, from: Point, first: Point, second: Point, to: Point) {
        let steps = segments(&[from, first, second, to]);
        let mut previous = from;
        for step in 1..=steps {
            let i = i64::from(step);
            let n = i64::from(steps);
            let j = n - i;
            let point = Point::new(
                div_round(
                    j * j * j * i64::from(from.x)
                        + 3 * j * j * i * i64::from(first.x)
                        + 3 * j * i * i * i64::from(second.x)
                        + i * i * i * i64::from(to.x),
                    n * n * n,
                ),
                div_round(
                    j * j * j * i64::from(from.y)
                        + 3 * j * j * i * i64::from(first.y)
                        + 3 * j * i * i * i64::from(second.y)
                        + i * i * i * i64::from(to.y),
                    n * n * n,
                ),
            );
            self.line(previous, point);
            previous = point;
        }
    }

    /// Fills the collected edges under the non-zero winding rule and returns the bitmap, or `None`
    /// when the canvas has no area.
    pub(crate) fn fill(&self) -> Option<Bitmap> {
        let width = u16::try_from(self.width).ok().and_then(NonZeroU16::new)?;
        let height = u16::try_from(self.height).ok()?;
        let stride = self.width.div_ceil(8);
        let mut bits = vec![0_u8; stride * self.height];
        let mut coverage = vec![0_u32; self.width];
        let mut crossings: Vec<(i32, i32)> = Vec::new();

        for row in 0..self.height {
            coverage.fill(0);
            let row_top = i32::try_from(row)
                .unwrap_or(i32::MAX)
                .saturating_mul(SUBPIXEL);
            for sample in 0..SAMPLES_PER_ROW {
                // The centre of the sample band, so a shape exactly filling the row is fully inked
                // and one grazing its edge is not.
                let y = row_top + (sample * SUBPIXEL * 2 + SUBPIXEL) / (SAMPLES_PER_ROW * 2);
                crossings.clear();
                for edge in &self.edges {
                    if y < edge.top.y || y >= edge.bottom.y {
                        continue;
                    }
                    let span = i64::from(edge.bottom.y - edge.top.y);
                    if span == 0 {
                        continue;
                    }
                    let run = i64::from(edge.bottom.x - edge.top.x);
                    let offset = i64::from(y - edge.top.y);
                    let x = i64::from(edge.top.x) + run * offset / span;
                    crossings.push((i32::try_from(x).unwrap_or(i32::MAX), edge.direction));
                }
                crossings.sort_by_key(|crossing| crossing.0);

                let mut winding = 0_i32;
                let mut span_start = 0_i32;
                for (x, direction) in &crossings {
                    if winding == 0 {
                        span_start = *x;
                    }
                    winding += *direction;
                    if winding == 0 {
                        add_span(&mut coverage, span_start, *x);
                    }
                }
            }

            let row_base = row * stride;
            for (column, ink) in coverage.iter().enumerate() {
                // Half-covered prints. A thermal head has no grey, so this is where the decision
                // has to be made, and the midpoint is the only choice that treats the two errors
                // alike.
                if ink.saturating_mul(2) < FULL_COVERAGE {
                    continue;
                }
                let index = row_base + column / 8;
                let mask = 0x80_u8 >> (column % 8);
                if let Some(byte) = bits.get_mut(index) {
                    *byte |= mask;
                }
            }
        }

        Some(Bitmap {
            width,
            height,
            bits,
        })
    }
}

/// Rounds a fixed-point division to nearest, away from zero on a tie, so a curve flattened forwards
/// and the same curve flattened backwards land on the same points — and so that scaling a glyph is
/// the same arithmetic wherever it happens.
pub(crate) fn div_round(numerator: i64, denominator: i64) -> i32 {
    if denominator == 0 {
        return 0;
    }
    let half = denominator / 2;
    let rounded = if numerator >= 0 {
        (numerator + half) / denominator
    } else {
        (numerator - half) / denominator
    };
    i32::try_from(rounded).unwrap_or(if rounded < 0 { i32::MIN } else { i32::MAX })
}

/// How many straight segments to flatten a curve into: enough that the control polygon's longest
/// leg is about a pixel, clamped so a degenerate curve cannot cost unbounded work.
fn segments(points: &[Point]) -> u32 {
    let mut length = 0_i64;
    for pair in points.windows(2) {
        let (Some(from), Some(to)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        length += i64::from((to.x - from.x).abs()) + i64::from((to.y - from.y).abs());
    }
    // One segment per half pixel of control-polygon length.
    let wanted = length / i64::from(SUBPIXEL / 2);
    u32::try_from(wanted.clamp(1, 64)).unwrap_or(1)
}

/// Adds a horizontal span's exact overlap with each pixel it touches.
fn add_span(coverage: &mut [u32], from: i32, to: i32) {
    if to <= from {
        return;
    }
    let limit = i32::try_from(coverage.len())
        .unwrap_or(i32::MAX)
        .saturating_mul(SUBPIXEL);
    let from = from.max(0);
    let to = to.min(limit);
    if to <= from {
        return;
    }
    // Both are non-negative: `from` was clamped to zero above and `to` is greater than it.
    let first = usize::try_from(from / SUBPIXEL).unwrap_or(0);
    let last = usize::try_from((to - 1) / SUBPIXEL).unwrap_or(0);
    for pixel in first..=last {
        let left = i32::try_from(pixel)
            .unwrap_or(i32::MAX)
            .saturating_mul(SUBPIXEL);
        let right = left.saturating_add(SUBPIXEL);
        let overlap = to.min(right) - from.max(left);
        if let Some(cell) = coverage.get_mut(pixel).filter(|_| overlap > 0) {
            *cell = cell.saturating_add(overlap.unsigned_abs());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Canvas, Point, SUBPIXEL};

    /// A point at whole-pixel coordinates.
    fn at(x: i32, y: i32) -> Point {
        Point::new(x * SUBPIXEL, y * SUBPIXEL)
    }

    fn rectangle(canvas: &mut Canvas, left: i32, top: i32, right: i32, bottom: i32) {
        canvas.line(at(left, top), at(right, top));
        canvas.line(at(right, top), at(right, bottom));
        canvas.line(at(right, bottom), at(left, bottom));
        canvas.line(at(left, bottom), at(left, top));
    }

    #[test]
    fn a_rectangle_fills_exactly_its_pixels() {
        let mut canvas = Canvas::new(8, 8);
        rectangle(&mut canvas, 2, 2, 6, 5);
        let bitmap = canvas.fill().expect("a canvas with area");

        for y in 0..8_u16 {
            for x in 0..8_u16 {
                let inside = (2..6).contains(&x) && (2..5).contains(&y);
                assert_eq!(
                    bitmap.pixel(x, y),
                    inside,
                    "pixel ({x}, {y}) should be {}",
                    if inside { "inked" } else { "blank" }
                );
            }
        }
    }

    #[test]
    fn a_hole_stays_open_under_the_winding_rule() {
        // The counter of an "o": an outer contour one way, an inner contour the other. A rasteriser
        // that ignored direction would fill the middle and print a blob.
        let mut canvas = Canvas::new(10, 10);
        rectangle(&mut canvas, 1, 1, 9, 9);
        // Reversed winding.
        canvas.line(at(3, 3), at(3, 7));
        canvas.line(at(3, 7), at(7, 7));
        canvas.line(at(7, 7), at(7, 3));
        canvas.line(at(7, 3), at(3, 3));
        let bitmap = canvas.fill().expect("a canvas with area");

        assert!(bitmap.pixel(2, 5), "the ring should be inked");
        assert!(!bitmap.pixel(5, 5), "the hole should stay open");
        assert!(
            bitmap.pixel(8, 5),
            "the far side of the ring should be inked"
        );
    }

    #[test]
    fn half_a_pixel_of_coverage_prints_and_less_does_not() {
        // The thresholding rule, pinned: a thermal head has no grey, so the midpoint decides.
        let mut canvas = Canvas::new(4, 1);
        // A band covering 60% of the row's height across the first two pixels.
        canvas.line(Point::new(0, 0), Point::new(2 * SUBPIXEL, 0));
        canvas.line(
            Point::new(2 * SUBPIXEL, 0),
            Point::new(2 * SUBPIXEL, (SUBPIXEL * 6) / 10),
        );
        canvas.line(
            Point::new(2 * SUBPIXEL, (SUBPIXEL * 6) / 10),
            Point::new(0, (SUBPIXEL * 6) / 10),
        );
        canvas.line(Point::new(0, (SUBPIXEL * 6) / 10), Point::new(0, 0));
        let bitmap = canvas.fill().expect("a canvas with area");
        assert!(bitmap.pixel(0, 0), "60% coverage prints");

        let mut thin = Canvas::new(4, 1);
        thin.line(Point::new(0, 0), Point::new(2 * SUBPIXEL, 0));
        thin.line(
            Point::new(2 * SUBPIXEL, 0),
            Point::new(2 * SUBPIXEL, (SUBPIXEL * 3) / 10),
        );
        thin.line(
            Point::new(2 * SUBPIXEL, (SUBPIXEL * 3) / 10),
            Point::new(0, (SUBPIXEL * 3) / 10),
        );
        thin.line(Point::new(0, (SUBPIXEL * 3) / 10), Point::new(0, 0));
        let bitmap = thin.fill().expect("a canvas with area");
        assert!(!bitmap.pixel(0, 0), "30% coverage does not print");
    }

    #[test]
    fn a_curve_flattens_the_same_way_every_time() {
        // The determinism claim this module is written for: the same outline, twice, is the same
        // bytes. With integer arithmetic that is a property rather than a hope.
        let render = || {
            let mut canvas = Canvas::new(16, 16);
            canvas.quad(at(2, 14), at(8, 0), at(14, 14));
            canvas.line(at(14, 14), at(2, 14));
            canvas.fill().expect("a canvas with area")
        };
        assert_eq!(render(), render());
        assert!(!render().is_blank(), "the arch should ink something");
    }

    #[test]
    fn nothing_drawn_is_a_blank_bitmap_rather_than_an_error() {
        // A line that shapes to no visible glyphs — a run of spaces — is legitimate.
        let bitmap = Canvas::new(8, 2).fill().expect("a canvas with area");
        assert!(bitmap.is_blank());
        assert_eq!(bitmap.height(), 2);
    }

    #[test]
    fn drawing_outside_the_canvas_is_clipped_rather_than_a_panic() {
        // Shaped text can overhang: a diacritic on the first glyph can sit left of the origin.
        let mut canvas = Canvas::new(4, 4);
        rectangle(&mut canvas, -20, -20, 20, 20);
        let bitmap = canvas.fill().expect("a canvas with area");
        for y in 0..4_u16 {
            for x in 0..4_u16 {
                assert!(bitmap.pixel(x, y), "the overhanging fill covers ({x}, {y})");
            }
        }
    }
}
