// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The menu-image pipeline ([ADR-0042](../../../docs/adr/0042-image-pipeline.md)).
//!
//! [`render`] turns a tenant-uploaded image into two JPEG renditions under hard byte budgets — a
//! ≤30 KB thumbnail and a ≤150 KB detail — so a store on a slow link loads a menu quickly and the
//! cloud stores bounded objects. JPEG size is not a closed form of dimension and quality, so each
//! rendition walks a descending `(max_edge, quality)` ladder and takes the first attempt at or under
//! budget; the ladders end aggressively enough that any real image fits, and an image that somehow
//! does not is a [`ImagePipelineError::Budget`] rather than an over-budget object. Pure and
//! deterministic — same bytes in, same bytes out — so it is unit-tested with no I/O.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageEncoder};

/// The thumbnail byte budget: 30 KB (`docs/roadmap.md` P7).
pub const THUMBNAIL_MAX_BYTES: usize = 30 * 1024;

/// The detail byte budget: 150 KB (`docs/roadmap.md` P7).
pub const DETAIL_MAX_BYTES: usize = 150 * 1024;

/// The descending `(max_edge_px, quality)` ladder for the thumbnail — the last rung is small enough
/// that any real image fits 30 KB.
const THUMBNAIL_LADDER: &[(u32, u8)] = &[(256, 80), (256, 60), (160, 55), (96, 45), (64, 35)];

/// The descending `(max_edge_px, quality)` ladder for the detail rendition.
const DETAIL_LADDER: &[(u32, u8)] = &[(1280, 82), (1024, 72), (800, 62), (640, 52), (480, 40)];

/// The two renditions a menu image reduces to, both JPEG.
#[derive(Debug, Clone)]
pub struct Renditions {
    /// The ≤30 KB thumbnail.
    pub thumbnail: Vec<u8>,
    /// The ≤150 KB detail rendition.
    pub detail: Vec<u8>,
}

/// Why the pipeline could not produce a rendition.
#[derive(Debug, thiserror::Error)]
pub enum ImagePipelineError {
    /// The uploaded bytes did not decode as a supported image (a bad or unsupported upload).
    #[error("the uploaded image could not be decoded: {0}")]
    Decode(#[source] image::ImageError),
    /// Encoding a rendition failed — an internal invariant breach, since the input decoded.
    #[error("encoding a rendition failed: {0}")]
    Encode(#[source] image::ImageError),
    /// Even the smallest ladder rung exceeded the budget (astronomically unlikely for a real image).
    #[error(
        "could not fit the image within {budget} bytes (smallest attempt was {smallest} bytes)"
    )]
    Budget {
        /// The byte budget that could not be met.
        budget: usize,
        /// The size of the smallest attempt produced.
        smallest: usize,
    },
}

/// Renders `input` into a thumbnail and a detail rendition, each within its byte budget.
///
/// # Errors
///
/// [`ImagePipelineError::Decode`] if the bytes are not a supported image;
/// [`ImagePipelineError::Encode`] if a rendition cannot be encoded; [`ImagePipelineError::Budget`] if
/// no ladder rung fits the budget.
pub fn render(input: &[u8]) -> Result<Renditions, ImagePipelineError> {
    let image = image::load_from_memory(input).map_err(ImagePipelineError::Decode)?;
    let thumbnail = fit_within(&image, THUMBNAIL_LADDER, THUMBNAIL_MAX_BYTES)?;
    let detail = fit_within(&image, DETAIL_LADDER, DETAIL_MAX_BYTES)?;
    Ok(Renditions { thumbnail, detail })
}

/// Walks `ladder` from largest/highest to smallest/lowest, returning the first JPEG at or under
/// `budget`, or a [`ImagePipelineError::Budget`] naming the smallest attempt if none fit.
fn fit_within(
    image: &DynamicImage,
    ladder: &[(u32, u8)],
    budget: usize,
) -> Result<Vec<u8>, ImagePipelineError> {
    let mut smallest: Option<Vec<u8>> = None;
    for &(max_edge, quality) in ladder {
        // `resize` fits the image within `max_edge × max_edge`, preserving aspect ratio, and never
        // upscales past the source, so a small upload stays small.
        let scaled = image.resize(max_edge, max_edge, FilterType::Lanczos3);
        let bytes = encode_jpeg(&scaled, quality)?;
        if bytes.len() <= budget {
            return Ok(bytes);
        }
        smallest = Some(match smallest {
            Some(previous) if previous.len() <= bytes.len() => previous,
            _ => bytes,
        });
    }
    Err(ImagePipelineError::Budget {
        budget,
        smallest: smallest.map_or(0, |bytes| bytes.len()),
    })
}

/// Encodes `image` as JPEG at `quality`, flattening to RGB (JPEG has no alpha).
fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, ImagePipelineError> {
    let rgb = image.to_rgb8();
    let mut buffer = Vec::new();
    JpegEncoder::new_with_quality(&mut buffer, quality)
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(ImagePipelineError::Encode)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::{DETAIL_MAX_BYTES, THUMBNAIL_MAX_BYTES, render};

    use image::{ExtendedColorType, ImageEncoder};

    /// A synthetic 600×400 RGB gradient, encoded to PNG bytes — a stand-in upload, no fixture file.
    fn sample_png() -> Vec<u8> {
        let image = image::RgbImage::from_fn(600, 400, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap_or(0),
                u8::try_from(y % 256).unwrap_or(0),
                u8::try_from((x + y) % 256).unwrap_or(0),
            ])
        });
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(image.as_raw(), 600, 400, ExtendedColorType::Rgb8)
            .expect("encode the sample png");
        png
    }

    #[test]
    fn render_produces_two_jpeg_renditions_within_budget() {
        let renditions = render(&sample_png()).expect("render");

        assert!(
            renditions.thumbnail.len() <= THUMBNAIL_MAX_BYTES,
            "thumbnail {} bytes exceeds the 30 KB budget",
            renditions.thumbnail.len()
        );
        assert!(
            renditions.detail.len() <= DETAIL_MAX_BYTES,
            "detail {} bytes exceeds the 150 KB budget",
            renditions.detail.len()
        );

        // Both renditions decode as valid images, and the thumbnail fits its 256-px bound.
        let thumbnail = image::load_from_memory(&renditions.thumbnail).expect("thumbnail decodes");
        assert!(
            thumbnail.width() <= 256 && thumbnail.height() <= 256,
            "the thumbnail is within its pixel bound"
        );
        assert!(
            image::load_from_memory(&renditions.detail).is_ok(),
            "the detail rendition decodes"
        );
    }

    #[test]
    fn render_rejects_bytes_that_are_not_an_image() {
        let outcome = render(b"this is plainly not an image");
        assert!(
            matches!(outcome, Err(super::ImagePipelineError::Decode(_))),
            "a non-image upload is a clean decode error, not a panic"
        );
    }
}
