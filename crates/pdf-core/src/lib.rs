//! Core PDF manipulation for Foolscap.
//!
//! This crate holds all of the logic and none of the presentation. The rules
//! that keep it usable from both the CLI and, later, the GTK front end:
//!
//! - Fallible functions return [`Result`], never panic on user input.
//! - Nothing here prints. Diagnostics go through `tracing`; the caller installs
//!   a subscriber.
//! - Nothing here calls [`std::process::exit`].
//! - Long operations report through a [`ProgressFn`] callback rather than
//!   driving their own progress display.

#![forbid(unsafe_code)]

mod assemble;
#[cfg(feature = "convert")]
pub mod convert;
pub mod document;
pub mod error;
#[cfg(feature = "ocr")]
pub mod ocr;
pub mod ops;
pub mod pages;
mod placement;
pub mod progress;
#[cfg(feature = "render")]
pub mod render;
#[cfg(any(feature = "convert", feature = "ocr"))]
mod subprocess;
mod text;

pub use document::{Document, Metadata};
pub use error::{PdfError, Result};
#[cfg(feature = "ocr")]
pub use ocr::{ocr, ocr_with_progress, OcrOptions, OcrReport};
pub use ops::{
    arrange, compress, compress_with_progress, crop, crop_margins, crop_margins_with_progress,
    crop_with_progress, delete, encrypt, extract_images, filter, filter_with_progress, flatten,
    flatten_with_progress, merge, merge_with_progress, move_pages, optimize, redact, redact_find,
    redact_find_with_progress, redact_with_progress, repair, repair_loaded, repair_with,
    reset_crop, rotate, scale_to, scale_to_with_progress, set_mediabox, set_mediabox_with_progress,
    split, split_plan, split_plan_with_options, split_with_options, split_with_progress, stamp,
    stamp_with_progress, strip, Anchor, CompressLevel, CompressReport, EncryptOptions, Filter,
    FilterReport, Fit, FlattenOptions, FlattenReport, Margins, MetadataEdit, OptimizeLevel,
    OptimizeReport, PageSize, Permissions, Point, Rect, RedactReport, RepairReport, SkipReason,
    SplitOptions, SplitSpec, StampContent, StampFont, StampLayer, StampOptions, StripReport,
    Stroke,
};
pub use pages::PageRange;
pub use progress::{Progress, ProgressFn};
#[cfg(feature = "render")]
pub use render::{PageRenderer, RenderedPage, Scale};

/// The version of `pdf-core`, taken from the crate manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Optional capabilities compiled into this build.
///
/// The GUI uses this to grey out features that are not available rather than
/// failing at invocation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Page rendering and thumbnails, via MuPDF.
    pub render: bool,
    /// Image and Office format conversion.
    pub convert: bool,
    /// OCR, via Tesseract.
    pub ocr: bool,
}

impl Capabilities {
    /// What this build supports.
    pub const fn current() -> Self {
        Self {
            render: cfg!(feature = "render"),
            convert: cfg!(feature = "convert"),
            ocr: cfg!(feature = "ocr"),
        }
    }
}
