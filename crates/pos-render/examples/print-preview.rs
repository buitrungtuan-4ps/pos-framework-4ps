// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What this box would send to a printer, on a screen instead of on paper.
//!
//! [ADR-0102](../../../docs/adr/0102-printing-any-script.md) makes the store draw the lines its
//! printer's code page cannot spell. Whether that works depends on which fonts the *deployment*
//! installed, which is a per-box fact no test in CI can settle — so this renders whatever lines you
//! give it, with whatever fonts this machine has, and shows the result.
//!
//! Run it before a pilot, on the box the store will use:
//!
//! ```text
//! cargo run -p pos-render --example print-preview -- "Phở bò tái nạm" "四種のチーズピザ" "पनीर टिक्का पिज़्ज़ा"
//! ```
//!
//! With no arguments it renders one line per script family the coverage probe knows about, which
//! answers "will this box print a Japanese ticket" without anyone typing kanji into a terminal.
//!
//! It prints, per line: the scripts the loaded faces cover, the raster's size, any character no
//! face covered (drawn as the font's own "no such character" box), and the raster itself as text —
//! one character per printer dot, so what you see is what the head would burn.
//!
//! Writing a `.pbm` beside it is optional and off by default: pass `--pbm <dir>` to get files an
//! image viewer opens.

// A terminal tool, not a service: its entire output *is* stdout, so the workspace's
// "log through tracing, because logs travel to the cloud" rule does not apply here. Scoped to this
// example rather than relaxed in `clippy.toml`, so the ban still holds everywhere it should.
#![expect(
    clippy::print_stdout,
    clippy::disallowed_macros,
    reason = "this example exists to print a raster to a terminal"
)]

use std::num::NonZeroU16;
use std::path::PathBuf;

use pos_ports::printer::TextStyle;
use pos_render::{Bitmap, FontLibrary, TextRenderer};

/// 80 mm at 203 dpi — the width `pos_edge::printing` assumes until the console sends a real one.
const DOTS: NonZeroU16 = NonZeroU16::new(576).expect("576 is not zero");
/// A comfortable receipt body at that resolution.
const SIZE: NonZeroU16 = NonZeroU16::new(24).expect("24 is not zero");

/// One line per script family, so a bare run answers the question a pilot actually asks.
const SAMPLES: &[(&str, &str)] = &[
    ("Latin", "Burrata di Bufala  285,000"),
    ("Vietnamese", "Phở bò tái nạm — ít hành, thêm ớt"),
    ("Japanese", "四種のチーズピザ（Lサイズ）"),
    ("Devanagari", "पनीर टिक्का पिज़्ज़ा"),
    ("Thai", "ผัดไทยกุ้งสด"),
    ("Arabic", "بيتزا بالجبنة"),
];

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // `--pbm <dir>` is pulled out first so the rest of the arguments are just lines to draw.
    let mut pbm_dir: Option<PathBuf> = None;
    if let Some(at) = args.iter().position(|a| a == "--pbm") {
        args.remove(at);
        if at < args.len() {
            pbm_dir = Some(PathBuf::from(args.remove(at)));
        }
    }

    let library = load_fonts();
    let coverage = library.coverage();
    let covered: Vec<&str> = coverage
        .iter()
        .filter(|row| row.covered)
        .map(|row| row.script)
        .collect();
    let missing: Vec<&str> = coverage
        .iter()
        .filter(|row| !row.covered)
        .map(|row| row.script)
        .collect();
    println!("faces loaded : {}", library.len());
    println!("can print    : {}", join(&covered));
    println!("cannot print : {}", join(&missing));
    if library.is_empty() {
        println!("\nNo font was found. This is the deployment failure ADR-0102 warns about:");
        println!("the box has the code and not the glyphs. Install a font package and re-run.");
        return;
    }

    let renderer = TextRenderer::new(library, SIZE);
    let style = TextStyle::default();

    let lines: Vec<(String, String)> = if args.is_empty() {
        SAMPLES
            .iter()
            .map(|(name, line)| ((*name).to_owned(), (*line).to_owned()))
            .collect()
    } else {
        args.iter()
            .enumerate()
            .map(|(index, line)| (format!("argument {}", index + 1), line.clone()))
            .collect()
    };

    for (label, line) in lines {
        println!("\n=== {label} ===");
        match renderer.render(&line, style, DOTS) {
            Ok(drawn) => {
                println!(
                    "{} x {} dots{}",
                    drawn.bitmap.width(),
                    drawn.bitmap.height(),
                    if drawn.substituted.is_empty() {
                        String::new()
                    } else {
                        // The characters, never the line: a print document can carry a buyer's name
                        // and tax code, and this output is meant to be pasteable into a ticket.
                        format!(
                            "  ·  no face covers: {}",
                            drawn.substituted.iter().collect::<String>()
                        )
                    }
                );
                print_raster(&drawn.bitmap);
                if let Some(dir) = pbm_dir.as_ref() {
                    write_pbm(dir, &label, &drawn.bitmap);
                }
            }
            Err(error) => println!("could not render: {error}"),
        }
    }
}

/// The platform font directories `pos_edge::config` defaults to, plus anything under them.
fn load_fonts() -> FontLibrary {
    let mut library = FontLibrary::new();
    let roots: &[&str] = if cfg!(windows) {
        &[r"C:\Windows\Fonts"]
    } else {
        &[
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/System/Library/Fonts",
        ]
    };
    for root in roots {
        let _ = library.add_directory(std::path::Path::new(root));
    }
    library
}

/// Script names as one line, or a dash when there are none.
fn join(names: &[&str]) -> String {
    if names.is_empty() {
        "—".to_owned()
    } else {
        names.join(", ")
    }
}

/// The raster as text, one character per dot, trimmed to the ink so a 576-dot row still fits a
/// terminal.
fn print_raster(bitmap: &Bitmap) {
    let Some((left, right)) = inked_columns(bitmap) else {
        println!("(blank — the line shaped to no visible glyph)");
        return;
    };
    for y in 0..bitmap.height() {
        let row: String = (left..=right)
            .map(|x| if bitmap.pixel(x, y) { '#' } else { '·' })
            .collect();
        println!("{row}");
    }
}

/// The first and last columns carrying ink, so the preview is not mostly blank paper.
fn inked_columns(bitmap: &Bitmap) -> Option<(u16, u16)> {
    let mut left = None;
    let mut right = 0;
    for x in 0..bitmap.width().get() {
        if (0..bitmap.height()).any(|y| bitmap.pixel(x, y)) {
            left.get_or_insert(x);
            right = x;
        }
    }
    left.map(|first| (first, right))
}

/// A P4 (binary) PBM: the smallest format that is exactly one bit per pixel, which is what the
/// bitmap already is — so this is a header plus the rows, with no conversion to get wrong.
fn write_pbm(dir: &std::path::Path, label: &str, bitmap: &Bitmap) {
    if std::fs::create_dir_all(dir).is_err() {
        println!("(could not create {})", dir.display());
        return;
    }
    let slug: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = dir.join(format!("{slug}.pbm"));
    let mut out = format!("P4\n{} {}\n", bitmap.width(), bitmap.height()).into_bytes();
    out.extend_from_slice(bitmap.bits());
    match std::fs::write(&path, out) {
        Ok(()) => println!("wrote {}", path.display()),
        Err(error) => println!("could not write {}: {error}", path.display()),
    }
}
