// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Shaping a line of text and rasterising it.
//!
//! # Why this is not "look each character up and draw it"
//!
//! Drawing one glyph per character is enough for Latin, and enough for Japanese, and wrong for a
//! great many scripts. In Devanagari `नमस्ते` the vowel sign on the second syllable is *written*
//! before the consonant it *follows*, two consonants join into a conjunct that has its own glyph,
//! and a mark reorders around the cluster; a per-character loop produces something a Hindi reader
//! cannot read. Getting that right is called shaping, and it is what [`rustybuzz`] — the HarfBuzz
//! algorithm in Rust — does here. The same pass also applies the OpenType mark positioning that
//! stacks a Vietnamese tone mark correctly over a vowel that already carries a diacritic.
//!
//! # The pipeline
//!
//! `text` → split into runs by which face covers each character → shape each run → lay the runs out
//! into rows that fit the paper → walk each glyph's outline into a [`Canvas`] → fill.
//!
//! Everything after the font parser's own coordinates is fixed-point integer arithmetic
//! ([`crate::raster`]), so the same line renders to the same bytes on every box in the fleet.

use core::num::NonZeroU16;
use core::fmt;

use pos_ports::printer::TextStyle;

use crate::font::{FontLibrary, LoadedFace};
use crate::raster::{Bitmap, Canvas, Point, SUBPIXEL, div_round};

/// A line could not be rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderError {
    /// No font is loaded, so nothing can be drawn at all.
    ///
    /// This is the deployment error ADR-0102 warns about: the box has the code and not the glyphs.
    NoFonts,
    /// The paper is too narrow to place a single glyph on.
    NoRoom {
        /// The width that was asked for, in printer dots.
        dots: u16,
    },
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFonts => f.write_str(
                "no font is loaded, so no text can be rendered — install a font package and set \
                 the font directory",
            ),
            Self::NoRoom { dots } => {
                write!(f, "{dots} dots is too narrow to render a line into")
            }
        }
    }
}

impl std::error::Error for RenderError {}

/// A rendered line, and what had to be substituted to render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLine {
    /// The raster.
    pub bitmap: Bitmap,
    /// Characters no loaded face covered, drawn as the font's "no such character" box.
    ///
    /// Not an error. A kitchen ticket reading `Phở [] size L` is still workable; a ticket that
    /// never printed is not. The caller logs these so the gap reaches an operator.
    pub substituted: Vec<char>,
}

/// Renders lines of text into rasters a thermal printer can take.
#[derive(Debug)]
pub struct TextRenderer {
    library: FontLibrary,
    size: NonZeroU16,
}

/// One glyph, positioned, with everything already converted to subpixels.
#[derive(Debug, Clone, Copy)]
struct Placed {
    face: usize,
    glyph: u32,
    advance: i32,
    x_offset: i32,
    y_offset: i32,
    /// Whether the source character was a space — that is, whether a row may end here.
    breakable: bool,
}

impl TextRenderer {
    /// A renderer drawing at `size` pixels per em.
    ///
    /// At the 203 dpi almost every thermal printer runs at, 24 px is a comfortable receipt body and
    /// 32 px is large. `size` is the em box, so the glyphs themselves are a little smaller.
    #[must_use]
    pub const fn new(library: FontLibrary, size: NonZeroU16) -> Self {
        Self { library, size }
    }

    /// The faces this renderer can draw with.
    #[must_use]
    pub const fn library(&self) -> &FontLibrary {
        &self.library
    }

    /// Pixels per em for ordinary text.
    #[must_use]
    pub const fn size(&self) -> NonZeroU16 {
        self.size
    }

    /// Renders one line into a raster `dots` wide, wrapping onto further rows when it does not fit.
    ///
    /// Wrapping rather than clipping is deliberate: a Vietnamese item name is longer than its
    /// English equivalent, and a kitchen ticket that silently loses the end of a modifier sends out
    /// the wrong dish.
    ///
    /// # Errors
    ///
    /// [`RenderError::NoFonts`] when the library is empty, [`RenderError::NoRoom`] when `dots` is
    /// too narrow for any glyph.
    pub fn render(
        &self,
        text: &str,
        style: TextStyle,
        dots: NonZeroU16,
    ) -> Result<RenderedLine, RenderError> {
        if self.library.is_empty() {
            return Err(RenderError::NoFonts);
        }
        let size = if style.double_size {
            self.size.get().saturating_mul(2)
        } else {
            self.size.get()
        };

        let mut substituted = Vec::new();
        let placed = self.shape(text, size, &mut substituted);

        let width_sub = i32::from(dots.get()).saturating_mul(SUBPIXEL);
        let rows = wrap(&placed, width_sub);
        let line_height = self.line_height(size);
        if line_height == 0 {
            return Err(RenderError::NoRoom { dots: dots.get() });
        }

        let height = rows.len().max(1).saturating_mul(usize::from(line_height));
        let mut canvas = Canvas::new(usize::from(dots.get()), height);
        // The baseline of the first row. Everything else is this plus whole line heights, so rows
        // sit on a common grid rather than drifting.
        let ascent = self.ascent(size);

        for (index, row) in rows.iter().enumerate() {
            let row_width = row.iter().map(|glyph| i64::from(glyph.advance)).sum::<i64>();
            let start = if style.centred {
                let slack = i64::from(width_sub) - row_width;
                i32::try_from(slack.max(0) / 2).unwrap_or(0)
            } else {
                0
            };
            let baseline = i32::try_from(index)
                .unwrap_or(0)
                .saturating_mul(i32::from(line_height))
                .saturating_mul(SUBPIXEL)
                .saturating_add(ascent);
            self.draw_row(&mut canvas, row, size, start, baseline, style.emphasised);
        }

        let bitmap = canvas
            .fill()
            .ok_or(RenderError::NoRoom { dots: dots.get() })?;
        Ok(RenderedLine {
            bitmap,
            substituted,
        })
    }

    /// Splits `text` into runs by covering face, shapes each, and returns every glyph with its
    /// metrics already in subpixels.
    fn shape(&self, text: &str, size: u16, substituted: &mut Vec<char>) -> Vec<Placed> {
        let mut placed = Vec::new();
        for (face_index, run) in self.runs(text, substituted) {
            let Some(face) = self.library.face(face_index) else {
                continue;
            };
            let Some(shaper) = face.shaper() else {
                continue;
            };
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(&run);
            // Script, direction and language from the text itself. This is what selects the Indic
            // or Arabic shaper rather than the default one.
            buffer.guess_segment_properties();
            let glyphs = rustybuzz::shape(&shaper, &[], buffer);

            let upem = i64::from(face.units_per_em());
            let scale = i64::from(size).saturating_mul(i64::from(SUBPIXEL));
            let infos = glyphs.glyph_infos();
            let positions = glyphs.glyph_positions();
            for (info, position) in infos.iter().zip(positions.iter()) {
                // A cluster indexes the run's bytes; a run that shaped to fewer glyphs than
                // characters (a conjunct) still points every glyph at a real character.
                let breakable = run
                    .get(usize::try_from(info.cluster).unwrap_or(0)..)
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(char::is_whitespace);
                placed.push(Placed {
                    face: face_index,
                    glyph: info.glyph_id,
                    advance: div_round(i64::from(position.x_advance).saturating_mul(scale), upem),
                    x_offset: div_round(i64::from(position.x_offset).saturating_mul(scale), upem),
                    y_offset: div_round(i64::from(position.y_offset).saturating_mul(scale), upem),
                    breakable,
                });
            }
        }
        placed
    }

    /// Groups `text` into the longest runs that one face can render, recording any character no
    /// face covers. An uncovered character joins the first face's run and draws its `.notdef` box.
    fn runs(&self, text: &str, substituted: &mut Vec<char>) -> Vec<(usize, String)> {
        let mut runs: Vec<(usize, String)> = Vec::new();
        for character in text.chars() {
            let face = self.library.face_for(character).unwrap_or_else(|| {
                if !character.is_whitespace()
                    && !character.is_control()
                    && !substituted.contains(&character)
                {
                    substituted.push(character);
                }
                0
            });
            match runs.last_mut() {
                Some(last) if last.0 == face => last.1.push(character),
                _ => runs.push((face, character.to_string())),
            }
        }
        runs
    }

    /// The tallest ascent among the loaded faces, in subpixels — where the first baseline sits.
    fn ascent(&self, size: u16) -> i32 {
        let scale = i64::from(size).saturating_mul(i64::from(SUBPIXEL));
        self.library
            .face(0)
            .map(|face| {
                div_round(
                    i64::from(face.ascender()).saturating_mul(scale),
                    i64::from(face.units_per_em()),
                )
            })
            .unwrap_or_default()
    }

    /// The distance from one row's baseline to the next, in whole pixels.
    fn line_height(&self, size: u16) -> u16 {
        let scale = i64::from(size).saturating_mul(i64::from(SUBPIXEL));
        let Some(face) = self.library.face(0) else {
            return 0;
        };
        let span = i64::from(face.ascender()) - i64::from(face.descender());
        let height = div_round(span.saturating_mul(scale), i64::from(face.units_per_em()));
        // Back to whole pixels, rounded up so two rows never overlap, and never zero.
        u16::try_from(height.max(SUBPIXEL).div_euclid(SUBPIXEL) + 1).unwrap_or(u16::MAX)
    }

    /// Draws one row of glyphs onto the canvas.
    fn draw_row(
        &self,
        canvas: &mut Canvas,
        row: &[Placed],
        size: u16,
        start: i32,
        baseline: i32,
        emphasised: bool,
    ) {
        // Faux bold: the same outline drawn again a fraction to the right. Thermal printers have no
        // second weight to switch to, and shipping a bold face for every script would double what
        // an operator has to install.
        let smear = if emphasised {
            (i32::from(size).saturating_mul(SUBPIXEL) / 24).max(SUBPIXEL / 2)
        } else {
            0
        };
        let mut pen = start;
        for glyph in row {
            let Some(face) = self.library.face(glyph.face) else {
                continue;
            };
            let Some(outlines) = face.outlines() else {
                continue;
            };
            let x = pen.saturating_add(glyph.x_offset);
            let y = baseline.saturating_sub(glyph.y_offset);
            draw_glyph(canvas, &outlines, face, glyph.glyph, size, x, y);
            if smear > 0 {
                draw_glyph(
                    canvas,
                    &outlines,
                    face,
                    glyph.glyph,
                    size,
                    x.saturating_add(smear),
                    y,
                );
            }
            pen = pen.saturating_add(glyph.advance);
        }
    }
}

/// Breaks a shaped line into rows no wider than `width`, preferring to break at a space.
fn wrap(placed: &[Placed], width: i32) -> Vec<Vec<Placed>> {
    if placed.is_empty() {
        return vec![Vec::new()];
    }
    let mut rows = Vec::new();
    let mut row: Vec<Placed> = Vec::new();
    let mut used = 0_i64;
    for glyph in placed {
        let next = used.saturating_add(i64::from(glyph.advance));
        if !row.is_empty() && next > i64::from(width) {
            // Break after the last space in the row if there is one, so words stay whole.
            let split = row.iter().rposition(|candidate| candidate.breakable);
            match split {
                Some(index) if index + 1 < row.len() => {
                    let carried = row.split_off(index + 1);
                    rows.push(core::mem::take(&mut row));
                    used = carried.iter().map(|g| i64::from(g.advance)).sum();
                    row = carried;
                }
                _ => {
                    rows.push(core::mem::take(&mut row));
                    used = 0;
                }
            }
        }
        used = used.saturating_add(i64::from(glyph.advance));
        row.push(*glyph);
    }
    rows.push(row);
    rows
}

/// Walks one glyph's outline into the canvas, scaled and positioned.
fn draw_glyph(
    canvas: &mut Canvas,
    outlines: &ttf_parser::Face<'_>,
    face: &LoadedFace,
    glyph: u32,
    size: u16,
    pen_x: i32,
    baseline_y: i32,
) {
    let Ok(glyph_id) = u16::try_from(glyph) else {
        return;
    };
    let mut sink = OutlineSink {
        canvas,
        scale: i64::from(size).saturating_mul(i64::from(SUBPIXEL)),
        units_per_em: i64::from(face.units_per_em()),
        pen_x,
        baseline_y,
        start: None,
        current: None,
    };
    outlines.outline_glyph(ttf_parser::GlyphId(glyph_id), &mut sink);
    sink.close_open_contour();
}

/// Receives a glyph's outline from the font parser and lays it into the canvas.
///
/// This is the boundary the crate's `clippy.toml` exists for: `ttf_parser::OutlineBuilder` speaks
/// `f32`, and every coordinate is rounded to a whole font design unit here and never computed with
/// as a float. A TrueType glyph's coordinates are integers already, so the rounding discards
/// nothing; a CFF glyph's may be fractional, and 1/2048 of an em is a fiftieth of a printer dot.
struct OutlineSink<'a> {
    canvas: &'a mut Canvas,
    scale: i64,
    units_per_em: i64,
    pen_x: i32,
    baseline_y: i32,
    start: Option<Point>,
    current: Option<Point>,
}

impl OutlineSink<'_> {
    /// Font units to a device point, y flipped and the pen applied.
    fn point(&self, x: f32, y: f32) -> Point {
        let x = div_round(
            i64::from(unit(x)).saturating_mul(self.scale),
            self.units_per_em,
        );
        let y = div_round(
            i64::from(unit(y)).saturating_mul(self.scale),
            self.units_per_em,
        );
        Point::new(
            self.pen_x.saturating_add(x),
            self.baseline_y.saturating_sub(y),
        )
    }

    /// Closes a contour the font left open. A well-formed glyph closes its own, but the fill rule
    /// needs closed contours and an unclosed one would leak ink across the row.
    fn close_open_contour(&mut self) {
        if let (Some(start), Some(current)) = (self.start, self.current)
            && start != current
        {
            self.canvas.line(current, start);
        }
        self.start = None;
        self.current = None;
    }
}

impl ttf_parser::OutlineBuilder for OutlineSink<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close_open_contour();
        let point = self.point(x, y);
        self.start = Some(point);
        self.current = Some(point);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        if let Some(from) = self.current {
            self.canvas.line(from, point);
        }
        self.current = Some(point);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.point(x1, y1);
        let point = self.point(x, y);
        if let Some(from) = self.current {
            self.canvas.quad(from, control, point);
        }
        self.current = Some(point);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let first = self.point(x1, y1);
        let second = self.point(x2, y2);
        let point = self.point(x, y);
        if let Some(from) = self.current {
            self.canvas.cubic(from, first, second, point);
        }
        self.current = Some(point);
    }

    fn close(&mut self) {
        self.close_open_contour();
    }
}

/// One font design unit from the parser's `f32`.
///
/// The only float-to-integer conversion in the crate, and deliberately the only one: everything
/// downstream of it is exact.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to the coordinate range a font can express before the cast, so the \
              truncation the lint warns about cannot happen"
)]
fn unit(value: f32) -> i32 {
    if value.is_nan() {
        return 0;
    }
    // Font coordinates live well inside this; a value outside it is a corrupt glyph, and clamping
    // draws something wrong rather than panicking mid-service.
    value.clamp(-65536.0, 65536.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::{RenderError, TextRenderer};
    use crate::font::FontLibrary;
    use core::num::NonZeroU16;
    use pos_ports::printer::TextStyle;
    use std::path::Path;

    fn dots(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).expect("positive")
    }

    /// The 80 mm paper almost every counter printer uses, at 203 dpi.
    const EIGHTY_MM: u16 = 576;

    fn on_dejavu() -> Option<TextRenderer> {
        let path = Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf");
        if !path.exists() {
            return None;
        }
        let mut library = FontLibrary::new();
        library.add_file(path).ok()?;
        Some(TextRenderer::new(library, dots(24)))
    }

    /// How many pixels are inked, as a crude "did anything render" measure.
    fn ink(bitmap: &crate::Bitmap) -> u32 {
        bitmap
            .bits()
            .iter()
            .map(|byte| byte.count_ones())
            .sum()
    }

    #[test]
    fn a_renderer_with_no_fonts_says_so_rather_than_printing_nothing() {
        let bare = TextRenderer::new(FontLibrary::new(), dots(24));
        assert_eq!(
            bare.render("Total", TextStyle::default(), dots(EIGHTY_MM)),
            Err(RenderError::NoFonts)
        );
    }

    #[test]
    fn vietnamese_renders() {
        // The line that could not be printed before this crate existed.
        let Some(renderer) = on_dejavu() else { return };
        let line = renderer
            .render("Phở bò tái nạm", TextStyle::default(), dots(EIGHTY_MM))
            .expect("a line with fonts loaded");
        assert!(line.substituted.is_empty(), "every character has a glyph");
        assert!(!line.bitmap.is_blank(), "something should be inked");
        assert_eq!(line.bitmap.width().get(), EIGHTY_MM);
    }

    #[test]
    fn a_tone_mark_puts_more_ink_on_the_page_than_the_bare_vowel() {
        // The diacritics are the whole point: if shaping dropped them, "Phơ" and "Phở" would be the
        // same picture, and the kitchen would read the wrong dish.
        let Some(renderer) = on_dejavu() else { return };
        let plain = renderer
            .render("Pho", TextStyle::default(), dots(EIGHTY_MM))
            .expect("renders");
        let marked = renderer
            .render("Phở", TextStyle::default(), dots(EIGHTY_MM))
            .expect("renders");
        assert!(
            ink(&marked.bitmap) > ink(&plain.bitmap),
            "the tone mark and horn should add ink"
        );
    }

    #[test]
    fn a_character_no_face_covers_is_substituted_and_reported_rather_than_failing() {
        let Some(renderer) = on_dejavu() else { return };
        let line = renderer
            .render("Sushi 寿司", TextStyle::default(), dots(EIGHTY_MM))
            .expect("the line still renders");
        assert_eq!(line.substituted, vec!['寿', '司']);
        assert!(
            !line.bitmap.is_blank(),
            "the Latin part must still print — a ticket with a box beats no ticket"
        );
    }

    #[test]
    fn a_long_line_wraps_instead_of_losing_its_end() {
        let Some(renderer) = on_dejavu() else { return };
        let short = renderer
            .render("Pizza", TextStyle::default(), dots(EIGHTY_MM))
            .expect("renders");
        let long = renderer
            .render(
                "Pizza bốn mùa với phô mai burrata, cà chua bi và húng quế tươi hái trong ngày",
                TextStyle::default(),
                dots(EIGHTY_MM),
            )
            .expect("renders");
        assert!(
            long.bitmap.height() > short.bitmap.height(),
            "the long line should occupy more rows ({} vs {})",
            long.bitmap.height(),
            short.bitmap.height()
        );
        assert_eq!(long.bitmap.width().get(), EIGHTY_MM, "and stay on the paper");
    }

    #[test]
    fn double_size_is_taller_and_emphasis_is_heavier() {
        let Some(renderer) = on_dejavu() else { return };
        let plain = renderer
            .render("TOTAL", TextStyle::default(), dots(EIGHTY_MM))
            .expect("renders");
        let big = renderer
            .render(
                "TOTAL",
                TextStyle {
                    double_size: true,
                    ..TextStyle::default()
                },
                dots(EIGHTY_MM),
            )
            .expect("renders");
        assert!(big.bitmap.height() > plain.bitmap.height());

        let bold = renderer
            .render(
                "TOTAL",
                TextStyle {
                    emphasised: true,
                    ..TextStyle::default()
                },
                dots(EIGHTY_MM),
            )
            .expect("renders");
        assert!(
            ink(&bold.bitmap) > ink(&plain.bitmap),
            "faux bold should lay down more ink"
        );
    }

    #[test]
    fn centring_moves_the_ink_rather_than_changing_it() {
        let Some(renderer) = on_dejavu() else { return };
        let left = renderer
            .render("4P's", TextStyle::default(), dots(EIGHTY_MM))
            .expect("renders");
        let centred = renderer
            .render(
                "4P's",
                TextStyle {
                    centred: true,
                    ..TextStyle::default()
                },
                dots(EIGHTY_MM),
            )
            .expect("renders");
        // The same glyphs, so about the same ink — not exactly the same, because moving a glyph
        // changes which subpixel phase its edges land on and the half-covered pixels round the
        // other way. A tenth is far tighter than a dropped or added glyph would be.
        let (before, after) = (ink(&left.bitmap), ink(&centred.bitmap));
        assert!(
            before.abs_diff(after) * 10 < before,
            "centring should move the glyphs, not change them ({before} vs {after} pixels)"
        );
        assert_ne!(left.bitmap, centred.bitmap, "but not in the same place");

        let leftmost = |bitmap: &crate::Bitmap| {
            (0..bitmap.width().get())
                .find(|x| (0..bitmap.height()).any(|y| bitmap.pixel(*x, y)))
                .unwrap_or(u16::MAX)
        };
        assert!(leftmost(&centred.bitmap) > leftmost(&left.bitmap));
    }

    #[test]
    fn the_same_line_renders_to_the_same_bytes_every_time() {
        // ADR-0102's determinism claim, as a test rather than a hope.
        let Some(renderer) = on_dejavu() else { return };
        let once = renderer
            .render("Bún chả Hà Nội", TextStyle::default(), dots(EIGHTY_MM))
            .expect("renders");
        let twice = renderer
            .render("Bún chả Hà Nội", TextStyle::default(), dots(EIGHTY_MM))
            .expect("renders");
        assert_eq!(once.bitmap, twice.bitmap);
    }

    #[test]
    fn an_empty_line_is_a_blank_row_rather_than_an_error() {
        let Some(renderer) = on_dejavu() else { return };
        let line = renderer
            .render("", TextStyle::default(), dots(EIGHTY_MM))
            .expect("renders");
        assert!(line.bitmap.is_blank());
        assert!(line.bitmap.height() > 0, "still occupies a row");
    }
}
