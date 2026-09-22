// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The faces a store can print with, and which characters each one covers.
//!
//! # Fonts are a deployment asset, not framework code
//!
//! A framework that embedded one font would ship a Vietnamese store several megabytes of kanji it
//! will never print, and would still not cover the next country. So [`FontLibrary`] is loaded from
//! files the deployment supplies — the packages named in `deploy/edge/README.md` — and this crate
//! carries the machinery rather than the glyphs ([ADR-0102](../../../docs/adr/0102-printing-any-script.md)).
//!
//! The cost of that choice is a box with no fonts installed, which could print nothing at all. It
//! is paid for by [`FontLibrary::coverage`]: the edge reports at boot which scripts it can actually
//! print, so a missing font is a line in the log and a field on the fleet screen rather than a
//! blank ticket at dinner service.
//!
//! # What the library refuses to hold
//!
//! A store's font directory is not a curated list. `/usr/share/fonts` on a plain Debian box carries
//! fifty faces, and `C:\Windows\Fonts` — the default on the Windows tills this framework targets —
//! carries several hundred, among them CJK collections of tens of megabytes each. The edge scans
//! that directory at boot and keeps what it finds resident for the life of the process.
//!
//! Three rules keep that from being the whole directory:
//!
//! 1. **One copy of each file.** A `.ttc` collection holds several faces in one file, and every face
//!    borrows from the same bytes. Measured on a plain Debian box, `wqy-zenhei.ttc` is 16.8 MB and
//!    holds three faces — 50.4 MB when each face owned its own copy, 16.8 MB now.
//! 2. **No face that cannot be reached.** `FontLibrary::face_for` answers with the **first** face
//!    covering a character, so a face whose every codepoint an earlier face already covers can never
//!    be chosen. Twelve Liberation faces — three families times four styles — all cover the same
//!    Latin, and eleven of them were unreachable the moment the first was loaded. Dropping them is
//!    not a heuristic: `face_for`, `covers`, `missing` and `coverage` all answer identically before
//!    and after, and the metrics in `text.rs` are read from face 0, which is never dropped.
//! 3. **An explicit ceiling**, [`MAX_FONT_BYTES`] and [`MAX_FACES`], because the two rules above
//!    bound the ordinary case and not the adversarial one. What was skipped is counted rather than
//!    silently lost: the edge logs [`FontLibrary::skipped`] beside the coverage line, so a store
//!    that really does need the hundred-and-first face learns it from a log rather than from a
//!    blank ticket.
//!
//! Measured on this crate's own CI box, a scan of `/usr/share/fonts` cost 139 MB of resident memory
//! before these rules and is the reason they exist: a till that trades on 2 GB of RAM cannot spend
//! a fifteenth of it on fonts for scripts the store does not print.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use skrifa::MetadataProvider as _;

/// A face could not be loaded.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadError {
    /// The file could not be read.
    Unreadable {
        /// Which file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The file was read but is not a font this build understands.
    NotAFont {
        /// Which file.
        path: PathBuf,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, source } => {
                write!(f, "could not read the font at {}: {source}", path.display())
            }
            Self::NotAFont { path } => {
                write!(f, "{} is not a TrueType or OpenType font", path.display())
            }
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } => Some(source),
            Self::NotAFont { .. } => None,
        }
    }
}

/// One loaded face.
pub(crate) struct LoadedFace {
    /// Where it came from, for diagnostics.
    name: String,
    /// The file's bytes. A face borrows from these, so they outlive every face built from them.
    ///
    /// Shared rather than owned, because a `.ttc` collection is one file holding several faces and
    /// each of them needs the whole of it: the table directory a face is built from points into
    /// bytes the other faces also use. Owning a copy apiece cost 50.4 MB for a 16.8 MB file on a
    /// plain Debian box, and a Windows till's CJK collections are larger again.
    data: Arc<[u8]>,
    /// Which face inside a collection file.
    index: u32,
    /// Font design units per em — the denominator for every scale in this crate.
    units_per_em: u16,
    /// Distance from the baseline to the top of the em box, in font units.
    ascender: i16,
    /// Distance from the baseline to the bottom, in font units. Negative in every real font.
    descender: i16,
    /// The codepoints this face has a glyph for, as sorted inclusive ranges. Ranges rather than a
    /// set because a CJK face covers tens of thousands of codepoints in a few hundred runs.
    coverage: Vec<(u32, u32)>,
}

impl fmt::Debug for LoadedFace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedFace")
            .field("name", &self.name)
            .field("index", &self.index)
            .field("units_per_em", &self.units_per_em)
            .field("ranges", &self.coverage.len())
            .finish_non_exhaustive()
    }
}

impl LoadedFace {
    /// Where this face came from.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Font design units per em.
    pub(crate) const fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Baseline to the top of the em box, in font units.
    pub(crate) const fn ascender(&self) -> i16 {
        self.ascender
    }

    /// Baseline to the bottom of the em box, in font units. Negative.
    pub(crate) const fn descender(&self) -> i16 {
        self.descender
    }

    /// Whether this face has a glyph for `codepoint`.
    pub(crate) fn covers(&self, codepoint: u32) -> bool {
        self.coverage
            .binary_search_by(|(first, last)| {
                if codepoint < *first {
                    core::cmp::Ordering::Greater
                } else if codepoint > *last {
                    core::cmp::Ordering::Less
                } else {
                    core::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// The parsed font, for outlines and for shaping. One parser serves both: `harfrust` and
    /// `skrifa` are both built on `read-fonts`, so this is the type each of them takes.
    pub(crate) fn font(&self) -> Option<skrifa::FontRef<'_>> {
        skrifa::FontRef::from_index(&self.data, self.index).ok()
    }
}

/// The faces available to render with, in fallback order.
///
/// The order is the order they were added: the first face that covers a character renders it, which
/// is the ordinary typographic fallback rule. Put the face you want ordinary Latin text in first.
#[derive(Debug, Default)]
pub struct FontLibrary {
    faces: Vec<LoadedFace>,
    /// Every codepoint some loaded face covers, as sorted inclusive ranges — the union of the
    /// `coverage` of everything in `faces`.
    ///
    /// Kept here rather than recomputed because it is what decides whether the next face is worth
    /// loading, and a directory scan asks that once per face.
    covered: Vec<(u32, u32)>,
    /// How many bytes of font file the library is holding, counting a `.ttc` once however many of
    /// its faces were kept.
    bytes_held: usize,
    /// How many faces parsed but were not kept: unreachable ones, and ones a ceiling refused.
    ///
    /// Counted rather than discarded silently. The first kind is free — those faces could never
    /// have been chosen — and the second is not, so an operator whose store really does need the
    /// face the ceiling refused has a number to find it by.
    skipped: usize,
}

/// The most font-file bytes a library will hold.
///
/// A ceiling rather than a guess at what a store needs. Latin plus two CJK collections is about
/// 45 MB, so this leaves room for a store printing several scripts and still refuses the several
/// hundred megabytes a Windows font directory would otherwise hand over.
pub const MAX_FONT_BYTES: usize = 64 * 1024 * 1024;

/// The most faces a library will hold.
///
/// Every kept face covers a codepoint no earlier face does, so this is far above what any store
/// prints — Unicode's scripts number in the low hundreds and a receipt uses a handful. It is here
/// because [`MAX_FONT_BYTES`] alone does not bound the list: one small collection can declare a
/// great many faces, and a `Vec` with no ceiling is what §2 of `AGENTS.md` forbids.
pub const MAX_FACES: usize = 64;

impl FontLibrary {
    /// An empty library.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no face has been loaded — a library that can render nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// How many faces are loaded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    /// The names of the loaded faces, in fallback order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.faces.iter().map(LoadedFace::name)
    }

    /// Loads every face in one font file, appending them in fallback order.
    ///
    /// A collection file (`.ttc`) contributes each of its faces. Returns how many were added.
    ///
    /// # Errors
    ///
    /// [`LoadError::Unreadable`] if the file cannot be read, [`LoadError::NotAFont`] if it parses as
    /// no face at all.
    pub fn add_file(&mut self, path: &Path) -> Result<usize, LoadError> {
        let data = std::fs::read(path).map_err(|source| LoadError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        let outcome = self.load(&name, &data);
        // `NotAFont` means the bytes parsed as no face at all, not that none of them were worth
        // keeping. A `.ttc` whose every face an earlier file already covers is a perfectly good
        // font; answering `Err` for it would have a caller log a corrupt-file warning about a file
        // that is fine, and `add_directory` treats an error as a reason to say nothing loaded.
        if outcome.parsed == 0 {
            return Err(LoadError::NotAFont {
                path: path.to_path_buf(),
            });
        }
        Ok(outcome.added)
    }

    /// Loads every face in an in-memory font file. Returns how many were added, which is zero when
    /// the bytes are not a font **and** when every face in them was already covered.
    pub fn add_bytes(&mut self, name: &str, data: &[u8]) -> usize {
        self.load(name, data).added
    }

    /// How many bytes of font file this library holds.
    ///
    /// A `.ttc` counts once however many of its faces were kept, because they share one copy. This
    /// is the figure [`MAX_FONT_BYTES`] bounds, and the one worth logging at boot: it is most of
    /// what a till's renderer costs in memory.
    #[must_use]
    pub const fn bytes_held(&self) -> usize {
        self.bytes_held
    }

    /// How many faces parsed but were not kept.
    ///
    /// Two reasons, deliberately counted together, because the operator's question is the same
    /// either way: *is a script missing because of this?* A face that covers nothing an earlier face
    /// does not could never have been chosen, so skipping it changes nothing; a face a ceiling
    /// refused might have mattered. Both answer that question the same way — check
    /// [`FontLibrary::coverage`], which is what the boot log prints beside this number.
    #[must_use]
    pub const fn skipped(&self) -> usize {
        self.skipped
    }

    /// Loads a font file's faces, keeping the ones that can be reached and fit under the ceilings.
    fn load(&mut self, name: &str, data: &[u8]) -> Loaded {
        let count = faces_in(data);
        let mut outcome = Loaded::default();
        // One allocation for the file, cloned as a handle by each face kept from it. Built before
        // the loop and dropped unused when no face is kept, so a file that contributes nothing
        // costs nothing.
        let shared: Arc<[u8]> = Arc::from(data);
        for index in 0..count {
            let Ok(face) = skrifa::FontRef::from_index(data, index) else {
                continue;
            };
            let metrics = face.metrics(
                skrifa::instance::Size::unscaled(),
                skrifa::instance::LocationRef::default(),
            );
            // A face with no glyph 0 has no outlines at all: a bitmap-only or metrics-only file,
            // which would report coverage and then render nothing.
            if metrics.units_per_em == 0
                || face.outline_glyphs().get(skrifa::GlyphId::new(0)).is_none()
            {
                continue;
            }
            outcome.parsed += 1;
            let coverage = coverage_of(&face);
            // The whole of the dedup rule. `face_for` answers with the first covering face, so a
            // face adding no codepoint to the union can never be the answer to anything.
            let merged = merge_ranges(&self.covered, &coverage);
            if covered_codepoints(&merged) == covered_codepoints(&self.covered) {
                self.skipped += 1;
                continue;
            }
            // Counted once per file, on the first face kept from it: the rest share these bytes.
            let cost = if outcome.added == 0 { data.len() } else { 0 };
            if self.faces.len() >= MAX_FACES
                || self.bytes_held.saturating_add(cost) > MAX_FONT_BYTES
            {
                self.skipped += 1;
                continue;
            }
            let loaded = LoadedFace {
                name: if count > 1 {
                    format!("{name}#{index}")
                } else {
                    name.to_owned()
                },
                data: Arc::clone(&shared),
                index,
                units_per_em: metrics.units_per_em,
                ascender: whole(metrics.ascent),
                descender: whole(metrics.descent),
                coverage,
            };
            self.faces.push(loaded);
            self.covered = merged;
            self.bytes_held = self.bytes_held.saturating_add(cost);
            outcome.added += 1;
        }
        outcome
    }

    /// Loads every font file under `directory`, including its subdirectories.
    ///
    /// Recursive because that is how font packages install: on Linux the `DejaVu` package writes to
    /// `/usr/share/fonts/truetype/dejavu/`, so a scan of `/usr/share/fonts/truetype` alone would
    /// find nothing and a store would silently have no fonts. Paths are visited in sorted order, so
    /// a library built twice from the same tree has the same fallback order.
    ///
    /// A file that is not a font is skipped rather than failing the load: font directories collect
    /// `README`s and licence files. Returns how many faces were added.
    ///
    /// # Errors
    ///
    /// [`LoadError::Unreadable`] if the directory itself cannot be listed. A subdirectory that
    /// cannot be listed is skipped — one unreadable corner of `/usr/share/fonts` should not stop a
    /// store printing.
    pub fn add_directory(&mut self, directory: &Path) -> Result<usize, LoadError> {
        let mut files = Vec::new();
        collect_fonts(directory, MAX_FONT_DEPTH, &mut files).map_err(|source| {
            LoadError::Unreadable {
                path: directory.to_path_buf(),
                source,
            }
        })?;
        files.sort();

        let mut added = 0;
        for path in files {
            if let Ok(data) = std::fs::read(&path) {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into(),
                );
                added += self.add_bytes(&name, &data);
            }
        }
        Ok(added)
    }

    /// Whether some loaded face can render `character`.
    #[must_use]
    pub fn covers(&self, character: char) -> bool {
        self.face_for(character).is_some()
    }

    /// The characters in `text` that no loaded face can render, in order of first appearance and
    /// without repeats. Empty when the whole string can be printed.
    #[must_use]
    pub fn missing(&self, text: &str) -> Vec<char> {
        let mut seen = BTreeSet::new();
        text.chars()
            .filter(|character| {
                !character.is_control()
                    && !character.is_whitespace()
                    && !self.covers(*character)
                    && seen.insert(*character)
            })
            .collect()
    }

    /// Which of the scripts this framework knows how to name can be printed.
    ///
    /// This is the boot diagnostic ADR-0102 leans on: an operator who installed no Japanese font
    /// learns it from a log line at start-up, not from a blank ticket during service.
    #[must_use]
    pub fn coverage(&self) -> Vec<ScriptCoverage> {
        PROBES
            .iter()
            .map(|(script, sample)| ScriptCoverage {
                script,
                covered: sample.chars().all(|character| self.covers(character)),
            })
            .collect()
    }

    /// The index of the first face covering `character`.
    pub(crate) fn face_for(&self, character: char) -> Option<usize> {
        let codepoint = u32::from(character);
        self.faces.iter().position(|face| face.covers(codepoint))
    }

    /// One loaded face.
    pub(crate) fn face(&self, index: usize) -> Option<&LoadedFace> {
        self.faces.get(index)
    }
}

/// Whether one named script can be printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCoverage {
    /// The script's name, for a log line or an operator screen.
    pub script: &'static str,
    /// Whether every character in this framework's sample for that script has a glyph.
    pub covered: bool,
}

/// One representative sample per script, chosen so that a face claiming the script but missing its
/// distinctive characters fails the probe. The Vietnamese sample is deliberately not plain Latin:
/// a font can cover ASCII and still lack `ệ`, which is exactly the failure that made a Vietnamese
/// ticket unprintable.
const PROBES: [(&str, &str); 8] = [
    ("Latin", "Aa1"),
    ("Vietnamese", "ệỗưừẩ"),
    ("Japanese", "寿司かなカナ"),
    ("Chinese", "菜单"),
    ("Korean", "한식"),
    ("Devanagari", "नमस्ते"),
    ("Thai", "อาหาร"),
    ("Arabic", "طعام"),
];

/// How deep to walk a font directory. Deep enough for every packaging layout in use, shallow enough
/// that a symlink loop or a misconfigured path cannot walk a whole filesystem at boot.
const MAX_FONT_DEPTH: u8 = 8;

/// Collects font files under `directory`, depth-first.
fn collect_fonts(directory: &Path, depth: u8, into: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if depth == 0 {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // `file_type` rather than `metadata`: it does not follow symlinks, so a link pointing back
        // up the tree is seen as a link and not descended into.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            // A subdirectory that cannot be read is skipped, not fatal.
            let _ = collect_fonts(&path, depth.saturating_sub(1), into);
        } else if path.extension().is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            extension == "ttf" || extension == "otf" || extension == "ttc"
        }) {
            into.push(path);
        }
    }
    Ok(())
}

/// The codepoints a face has a real glyph for, as sorted inclusive ranges.
fn coverage_of(face: &skrifa::FontRef<'_>) -> Vec<(u32, u32)> {
    let mut points: Vec<u32> = face
        .charmap()
        .mappings()
        // A `cmap` entry pointing at glyph 0 is the font saying "I do not have this", so it is not
        // coverage; keeping it would make `covers` answer true for a character that prints as a box.
        .filter(|(_, glyph)| glyph.to_u32() != 0)
        .map(|(codepoint, _)| codepoint)
        .collect();
    points.sort_unstable();
    points.dedup();

    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for codepoint in points {
        match ranges.last_mut() {
            Some(last) if last.1.saturating_add(1) == codepoint => last.1 = codepoint,
            _ => ranges.push((codepoint, codepoint)),
        }
    }
    ranges
}

/// What one font file contributed.
///
/// Two numbers because they answer different questions: `parsed` is whether the bytes were a font
/// at all, which is what [`LoadError::NotAFont`] reports, and `added` is how many faces the library
/// kept, which is what a caller counting coverage wants. Before the dedup rule they were the same
/// number, and collapsing them again would have a file whose faces are all already covered read as
/// a corrupt file.
#[derive(Debug, Default, Clone, Copy)]
struct Loaded {
    /// Faces that parsed and had outlines, whether or not they were kept.
    parsed: usize,
    /// Faces the library kept.
    added: usize,
}

/// How many codepoints a set of sorted inclusive ranges covers.
///
/// `u64` because the ranges are `u32` pairs and their sum can leave `u32` — a face covering the
/// whole plane would.
fn covered_codepoints(ranges: &[(u32, u32)]) -> u64 {
    ranges
        .iter()
        .map(|(first, last)| u64::from(*last) - u64::from(*first) + 1)
        .sum()
}

/// The union of two sets of sorted inclusive ranges, itself sorted and with touching ranges joined.
///
/// Joining ranges that merely touch — `(1, 5)` and `(6, 9)` becoming `(1, 9)` — matters because the
/// result is compared by codepoint count, and a set that kept them apart would still be correct but
/// would grow without bound over a directory scan.
fn merge_ranges(left: &[(u32, u32)], right: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(left.len() + right.len());
    let mut all: Vec<(u32, u32)> = Vec::with_capacity(left.len() + right.len());
    all.extend_from_slice(left);
    all.extend_from_slice(right);
    all.sort_unstable();
    for (first, last) in all {
        match merged.last_mut() {
            // `first <= joined.1 + 1` covers both overlap and adjacency; the saturating add is for
            // a range ending at `u32::MAX`, where nothing can be adjacent above it anyway.
            Some(joined) if first <= joined.1.saturating_add(1) => joined.1 = joined.1.max(last),
            _ => merged.push((first, last)),
        }
    }
    merged
}

/// How many faces a font file holds — more than one only for a collection (`.ttc`).
fn faces_in(data: &[u8]) -> u32 {
    skrifa::raw::FileRef::new(data).map_or(1, |file| match file {
        skrifa::raw::FileRef::Font(_) => 1,
        skrifa::raw::FileRef::Collection(collection) => collection.len(),
    })
}

/// A font-unit metric from `skrifa`'s `f32`, rounded to a whole design unit.
///
/// The same boundary rule as `text.rs`: a float may be named where the font parser hands one over,
/// and never computed with (ADR-0102).
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to the range a font metric can express before the cast"
)]
fn whole(value: f32) -> i16 {
    if value.is_nan() {
        return 0;
    }
    value.clamp(-32768.0, 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::{Arc, FontLibrary, covered_codepoints, merge_ranges};
    use std::path::Path;

    /// A face that is present wherever this repository's tests run: the `fonts-dejavu-core` package
    /// the edge image installs. A test needing a real face skips rather than fails when it is
    /// absent, because the font is a deployment asset and not part of the tree.
    pub(super) fn dejavu() -> Option<FontLibrary> {
        let path = Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf");
        if !path.exists() {
            return None;
        }
        let mut library = FontLibrary::new();
        library.add_file(path).ok()?;
        Some(library)
    }

    #[test]
    fn an_empty_library_covers_nothing_and_says_so() {
        let library = FontLibrary::new();
        assert!(library.is_empty());
        assert!(!library.covers('A'));
        assert_eq!(library.missing("Phở"), vec!['P', 'h', 'ở']);
        assert!(library.coverage().iter().all(|script| !script.covered));
    }

    #[test]
    fn bytes_that_are_not_a_font_add_no_faces() {
        let mut library = FontLibrary::new();
        assert_eq!(library.add_bytes("notes.txt", b"not a font"), 0);
        assert!(library.is_empty());
    }

    #[test]
    fn a_real_face_covers_vietnamese_and_reports_which_scripts_it_has() {
        let Some(library) = dejavu() else {
            return;
        };
        // The characters that made a Vietnamese ticket unprintable.
        assert!(library.covers('ệ'), "ệ should have a glyph");
        assert!(library.covers('ở'), "ở should have a glyph");
        assert!(library.missing("Phở bò tái nạm").is_empty());

        let coverage = library.coverage();
        let has = |script: &str| {
            coverage
                .iter()
                .find(|entry| entry.script == script)
                .is_some_and(|entry| entry.covered)
        };
        assert!(has("Latin"));
        assert!(has("Vietnamese"));
        // And it honestly reports the scripts it does not carry, rather than claiming everything.
        assert!(!has("Japanese"), "DejaVu Sans has no kanji");
        assert!(!has("Devanagari"), "DejaVu Sans has no Devanagari");
    }

    #[test]
    fn missing_names_the_characters_to_install_a_font_for() {
        let Some(library) = dejavu() else {
            return;
        };
        // Not an error, a list: this is what the operator needs to know.
        assert_eq!(library.missing("寿司 set"), vec!['寿', '司']);
        // Whitespace is never "missing"; it renders as an advance.
        assert!(library.missing("   ").is_empty());
    }

    #[test]
    fn coverage_ranges_are_sorted_and_searchable() {
        let Some(library) = dejavu() else {
            return;
        };
        let face = library.face(0).expect("one face");
        // Spot-check the boundaries of the block Vietnamese lives in.
        assert!(face.covers(u32::from('A')));
        assert!(face.covers(0x1EC7)); // ệ
        assert!(!face.covers(0x4E00)); // 一, which DejaVu does not carry
    }

    /// The dedup rule, stated as the thing an operator would notice: loading the same font twice
    /// costs nothing the second time.
    ///
    /// Written with one file loaded twice rather than with two different fonts, because it is the
    /// only shape that is deterministic on any box: whatever the file covers, the second copy
    /// covers exactly that and nothing more, which is precisely the condition the rule tests for.
    /// It is also not a contrived case — a store with `/usr/share/fonts` and `/usr/local/share/fonts`
    /// both in `font_directories`, one a symlink to the other, is a real deployment.
    #[test]
    fn a_face_that_adds_no_coverage_is_not_kept() {
        let path = Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf");
        if !path.exists() {
            return;
        }
        let mut library = FontLibrary::new();
        assert_eq!(library.add_file(path).expect("first load"), 1);
        let after_first = library.bytes_held();
        assert!(after_first > 0, "the first copy is held");

        assert_eq!(
            library.add_file(path).expect("second load"),
            0,
            "the second copy covers nothing the first does not"
        );
        assert_eq!(library.len(), 1, "and so is not kept");
        assert_eq!(library.skipped(), 1, "but is counted, not lost");
        assert_eq!(
            library.bytes_held(),
            after_first,
            "and costs no memory — which is the whole point"
        );

        // What the store can print is unchanged, which is what makes the skip safe.
        assert!(library.covers('ệ'));
        assert!(library.missing("Phở bò tái nạm").is_empty());
    }

    /// A second, *different* font is kept — so the rule above is dropping redundancy rather than
    /// everything after the first file.
    ///
    /// Skips when the box has no second font carrying a script `DejaVu` does not, because the
    /// assertion has no meaning without one.
    #[test]
    fn a_face_that_adds_coverage_is_kept() {
        let Some(mut library) = dejavu() else {
            return;
        };
        let Some(cjk) = ["/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.exists())
        else {
            return;
        };
        assert!(!library.covers('一'), "DejaVu has no kanji to start with");
        assert!(
            library.add_file(cjk).expect("load") > 0,
            "the CJK face adds coverage, so it is kept"
        );
        assert!(library.covers('一'), "and the store can now print it");
    }

    /// A collection file is held once, not once per face.
    ///
    /// The measurement that started this: `wqy-zenhei.ttc` is 16.8 MB and declares three faces, and
    /// a copy apiece cost 50.4 MB for one file. Asserted as "no more than the file", which holds
    /// however many of its faces this box's earlier fonts happen to make redundant.
    #[test]
    fn a_collection_is_held_once_however_many_of_its_faces_are_kept() {
        let path = Path::new("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc");
        if !path.exists() {
            return;
        }
        let size = std::fs::metadata(path).expect("stat").len();
        let mut library = FontLibrary::new();
        let added = library.add_file(path).expect("load");
        assert!(
            added > 1,
            "this fixture is a collection, so it contributes several faces"
        );
        assert_eq!(
            u64::try_from(library.bytes_held()).expect("fits"),
            size,
            "{added} faces share one copy of the file"
        );

        // The accounting above is a number this crate writes down; this is the thing itself. Two
        // faces of one collection must borrow the *same* allocation, or `bytes_held` would be a
        // comforting figure next to three copies of a 16.8 MB file.
        let first = library.face(0).expect("face 0");
        let second = library.face(1).expect("face 1");
        assert!(
            std::ptr::eq(Arc::as_ptr(&first.data), Arc::as_ptr(&second.data)),
            "the faces of one collection point at one copy of its bytes"
        );
    }

    #[test]
    fn the_union_of_two_range_sets_joins_the_ones_that_touch() {
        // Adjacent, so they become one: the count is what the dedup rule compares, and a set that
        // kept touching ranges apart would grow over a directory scan without covering more.
        assert_eq!(merge_ranges(&[(1, 5)], &[(6, 9)]), vec![(1, 9)]);
        // Overlapping.
        assert_eq!(merge_ranges(&[(1, 5)], &[(3, 9)]), vec![(1, 9)]);
        // Disjoint, and sorted whichever order they arrive in.
        assert_eq!(merge_ranges(&[(20, 21)], &[(1, 2)]), vec![(1, 2), (20, 21)]);
        // One wholly inside the other adds nothing, which is the case the skip is built on.
        assert_eq!(merge_ranges(&[(1, 9)], &[(3, 4)]), vec![(1, 9)]);
        assert_eq!(covered_codepoints(&[(1, 9)]), 9);
        // A range ending at the top of the space does not wrap.
        assert_eq!(
            merge_ranges(&[(u32::MAX - 1, u32::MAX)], &[(0, 0)]),
            vec![(0, 0), (u32::MAX - 1, u32::MAX)]
        );
    }
}
