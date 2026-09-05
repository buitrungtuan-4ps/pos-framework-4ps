// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Text rendering for thermal printers.

#![forbid(unsafe_code)]

pub mod font;
pub mod raster;
pub mod text;

pub use font::{FontLibrary, LoadError, ScriptCoverage};
pub use raster::Bitmap;
pub use text::{RenderError, RenderedLine, TextRenderer};
