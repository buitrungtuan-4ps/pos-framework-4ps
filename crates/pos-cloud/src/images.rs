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
//!
//! # Where it runs, and how many at once
//!
//! [`render`] is the most expensive thing the cloud does per request: a decode, then up to five
//! Lanczos3 resizes and JPEG encodes per rendition. Run on an async worker it holds that thread for
//! the whole walk, and a Tokio worker that is busy is not polling anything else on it — one upload
//! stalls unrelated requests that happen to share the thread. So the HTTP handler goes through
//! [`render_blocking`], which hands the work to the blocking pool.
//!
//! That alone would move the problem rather than bound it: the blocking pool is hundreds of threads,
//! and a browser uploading a folder of photos would put every one of them on a core that does not
//! exist, each holding a decoded bitmap. [`render_blocking`] therefore takes a permit first, from a
//! process-wide semaphore sized to the machine. An upload waits [`RENDER_QUEUE_WAIT`] for one — a
//! burst of a dozen is normal and queues — and is told the server is busy rather than waiting past
//! the point where its own client has given up.

use std::sync::OnceLock;
use std::time::Duration;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageEncoder};
use tokio::sync::Semaphore;

/// The thumbnail byte budget: 30 KB (`docs/roadmap.md` P7).
pub const THUMBNAIL_MAX_BYTES: usize = 30 * 1024;

/// The detail byte budget: 150 KB (`docs/roadmap.md` P7).
pub const DETAIL_MAX_BYTES: usize = 150 * 1024;

/// The descending `(max_edge_px, quality)` ladder for the thumbnail — the last rung is small enough
/// that any real image fits 30 KB.
const THUMBNAIL_LADDER: &[(u32, u8)] = &[(256, 80), (256, 60), (160, 55), (96, 45), (64, 35)];

/// The descending `(max_edge_px, quality)` ladder for the detail rendition.
const DETAIL_LADDER: &[(u32, u8)] = &[(1280, 82), (1024, 72), (800, 62), (640, 52), (480, 40)];

/// How long an upload waits for a rendering slot before the server admits it is busy.
///
/// Long enough that a console uploading a folder of photos queues rather than failing — a render is
/// well under a second, so a dozen at a cap of four clear inside this — and short enough that the
/// answer arrives while the browser is still listening for it. Refusing at the door instead would
/// turn the ordinary act of selecting several files into a row of red toasts.
pub const RENDER_QUEUE_WAIT: Duration = Duration::from_secs(5);

/// The most renders in flight at once, whatever the machine reports.
///
/// The work is CPU-bound, so the natural cap is the cores available; the ceiling keeps a large host
/// from turning the whole box over to image uploads, and the floor keeps a single-core container
/// able to serve one.
const RENDER_SLOT_CEILING: usize = 8;
const RENDER_SLOT_FLOOR: usize = 1;

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

/// Why an upload could not be rendered, as the HTTP layer sees it.
///
/// Separate from [`ImagePipelineError`], which stays what it was: everything that can be wrong with
/// the *image*. These two are what can be wrong with the *moment* — the server is already rendering
/// as many as it will, or the task carrying the work did not come back. Keeping them apart is what
/// lets the handler answer "your image is not usable" and "come back in a second" differently,
/// which is the whole point of a 429.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The image itself was the problem.
    #[error(transparent)]
    Pipeline(#[from] ImagePipelineError),
    /// No rendering slot came free inside [`RENDER_QUEUE_WAIT`].
    #[error("the server is already rendering {limit} uploads; none came free in {wait:?}")]
    Busy {
        /// The concurrent-render cap this process is running under.
        limit: usize,
        /// How long the upload waited for a slot.
        wait: Duration,
    },
    /// The blocking task panicked or was cancelled — an internal fault, not a bad upload.
    #[error("the rendering task did not return: {0}")]
    Join(#[source] tokio::task::JoinError),
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

/// Renders `input` on the blocking pool, under the process-wide concurrency cap.
///
/// This is what an HTTP handler calls. It takes a permit before it starts, so the number of images
/// being decoded at once is bounded by the machine rather than by how many clients happened to press
/// upload; an upload that cannot get one inside [`RENDER_QUEUE_WAIT`] comes back [`RenderError::Busy`]
/// for the caller to turn into a retryable refusal.
///
/// The permit is held for the whole render and released when this future resolves — including when
/// the blocking task panics, because the permit lives here rather than inside the task.
///
/// # Errors
///
/// [`RenderError::Pipeline`] for anything wrong with the image itself, [`RenderError::Busy`] if no
/// slot came free in time, [`RenderError::Join`] if the blocking task did not return.
pub async fn render_blocking<B>(input: B) -> Result<Renditions, RenderError>
where
    B: AsRef<[u8]> + Send + 'static,
{
    render_waiting_up_to(input, RENDER_QUEUE_WAIT).await
}

/// [`render_blocking`] with the queue wait named, so a test can drain the slots without sleeping
/// for the production wait.
async fn render_waiting_up_to<B>(input: B, wait: Duration) -> Result<Renditions, RenderError>
where
    B: AsRef<[u8]> + Send + 'static,
{
    let Ok(Ok(permit)) = tokio::time::timeout(wait, render_slots().acquire()).await else {
        // Either the wait elapsed, or the semaphore was closed — which nothing in this process does.
        // Both mean the same thing to the caller: no slot, try again.
        return Err(RenderError::Busy {
            limit: concurrent_render_limit(),
            wait,
        });
    };
    let outcome = tokio::task::spawn_blocking(move || render(input.as_ref())).await;
    drop(permit);
    match outcome {
        Ok(rendered) => rendered.map_err(RenderError::Pipeline),
        Err(join_error) => Err(RenderError::Join(join_error)),
    }
}

/// The process-wide rendering slots, sized once on first use.
fn render_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(concurrent_render_limit()))
}

/// How many renders this machine will run at once.
///
/// `available_parallelism` reports the cores this process may actually use — the cgroup quota in a
/// container, not the host's core count — which is the number that matters for CPU-bound work.
/// Where it cannot be determined, two: enough that one slow upload does not block the next, low
/// enough to be safe on a small box.
fn concurrent_render_limit() -> usize {
    std::thread::available_parallelism()
        .map_or(2, std::num::NonZeroUsize::get)
        .clamp(RENDER_SLOT_FLOOR, RENDER_SLOT_CEILING)
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
    use std::time::Duration;

    use super::{
        DETAIL_MAX_BYTES, RenderError, THUMBNAIL_MAX_BYTES, concurrent_render_limit, render,
        render_slots, render_waiting_up_to,
    };

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

    /// The cap, the refusal it produces, and the slot coming back afterwards — in one test, because
    /// the slots are process-wide and two tests draining them at once would fight.
    #[tokio::test]
    async fn an_upload_waits_for_a_slot_and_is_refused_only_while_there_is_none() {
        let limit = u32::try_from(concurrent_render_limit()).expect("the cap fits a u32");
        let held = render_slots()
            .acquire_many(limit)
            .await
            .expect("hold every rendering slot");

        // Every slot is taken, so this one waits out its (deliberately tiny) patience and is told
        // so. The distinction that matters: it is not an ImagePipelineError — the image is fine.
        let refused = render_waiting_up_to(sample_png(), Duration::from_millis(10)).await;
        match refused {
            Err(RenderError::Busy {
                limit: reported, ..
            }) => {
                assert_eq!(reported, concurrent_render_limit(), "it names the real cap");
            }
            other => panic!("expected a busy refusal while every slot is held, got {other:?}"),
        }

        drop(held);

        // And the cap is a queue, not a fuse: with the slots back, the same upload renders, and to
        // the same two bounded renditions the pure walk produces.
        let renditions = render_waiting_up_to(sample_png(), Duration::from_secs(5))
            .await
            .expect("render once a slot is free");
        assert!(renditions.thumbnail.len() <= THUMBNAIL_MAX_BYTES);
        assert!(renditions.detail.len() <= DETAIL_MAX_BYTES);
    }
}
