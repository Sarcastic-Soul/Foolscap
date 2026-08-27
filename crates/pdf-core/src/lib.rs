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
pub mod document;
pub mod error;
pub mod ops;
pub mod pages;
pub mod progress;
mod text;

pub use document::{Document, Metadata};
pub use error::{PdfError, Result};
pub use ops::{
    merge, merge_with_progress, optimize, rotate, split, split_plan, split_with_progress,
    MetadataEdit, OptimizeLevel, OptimizeReport, SplitSpec,
};
pub use pages::PageRange;
pub use progress::{Progress, ProgressFn};

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
