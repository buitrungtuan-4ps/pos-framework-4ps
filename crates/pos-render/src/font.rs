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

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

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
    data: Vec<u8>,
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

    /// A parsed outline reader for this face.
    pub(crate) fn outlines(&self) -> Option<ttf_parser::Face<'_>> {
        ttf_parser::Face::parse(&self.data, self.index).ok()
    }

    /// A shaper for this face.
    pub(crate) fn shaper(&self) -> Option<rustybuzz::Face<'_>> {
        rustybuzz::Face::from_slice(&self.data, self.index)
    }
}

/// The faces available to render with, in fallback order.
///
/// The order is the order they were added: the first face that covers a character renders it, which
/// is the ordinary typographic fallback rule. Put the face you want ordinary Latin text in first.
#[derive(Debug, Default)]
pub struct FontLibrary {
    faces: Vec<LoadedFace>,
}

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
        let name = path
            .file_name()
            .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
        let added = self.add_bytes(&name, &data);
        if added == 0 {
            return Err(LoadError::NotAFont {
                path: path.to_path_buf(),
            });
        }
        Ok(added)
    }

    /// Loads every face in an in-memory font file. Returns how many were added, which is zero when
    /// the bytes are not a font.
    pub fn add_bytes(&mut self, name: &str, data: &[u8]) -> usize {
        let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
        let mut added = 0;
        for index in 0..count {
            let Ok(face) = ttf_parser::Face::parse(data, index) else {
                continue;
            };
            // A face with no outlines is a bitmap-only or metrics-only file; it can report coverage
            // and then render nothing, which is worse than not loading it.
            if face.number_of_glyphs() == 0 || face.units_per_em() == 0 {
                continue;
            }
            let loaded = LoadedFace {
                name: if count > 1 {
                    format!("{name}#{index}")
                } else {
                    name.to_owned()
                },
                data: data.to_vec(),
                index,
                units_per_em: face.units_per_em(),
                ascender: face.ascender(),
                descender: face.descender(),
                coverage: coverage_of(&face),
            };
            self.faces.push(loaded);
            added += 1;
        }
        added
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
                let name = path
                    .file_name()
                    .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
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
        self.faces
            .iter()
            .position(|face| face.covers(codepoint))
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
fn collect_fonts(
    directory: &Path,
    depth: u8,
    into: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    if depth == 0 {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // `file_type` rather than `metadata`: it does not follow symlinks, so a link pointing back
        // up the tree is seen as a link and not descended into.
        let Ok(kind) = entry.file_type() else { continue };
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
///
/// A `cmap` entry pointing at glyph 0 is not coverage — it is the font saying "I do not have this"
/// — so those are dropped. Keeping them would make [`FontLibrary::covers`] answer `true` for a
/// character that prints as an empty box.
fn coverage_of(face: &ttf_parser::Face<'_>) -> Vec<(u32, u32)> {
    let mut points: Vec<u32> = Vec::new();
    if let Some(cmap) = face.tables().cmap {
        for subtable in cmap.subtables {
            if !subtable.is_unicode() {
                continue;
            }
            subtable.codepoints(|codepoint| points.push(codepoint));
        }
    }
    points.sort_unstable();
    points.dedup();
    points.retain(|codepoint| {
        char::from_u32(*codepoint)
            .and_then(|character| face.glyph_index(character))
            .is_some_and(|glyph| glyph.0 != 0)
    });

    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for codepoint in points {
        match ranges.last_mut() {
            Some(last) if last.1.saturating_add(1) == codepoint => last.1 = codepoint,
            _ => ranges.push((codepoint, codepoint)),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::FontLibrary;
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
}
