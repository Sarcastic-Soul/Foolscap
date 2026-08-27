use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::pages::PageRange;

/// How to divide a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitSpec {
    /// Extract the selected pages into one output file.
    Extract(PageRange),
    /// Burst into chunks of `n` pages each.
    Every(usize),
}

/// Split `input` according to `spec`, writing results into `out_dir`.
///
/// Returns the paths written, in order.
pub fn split(_input: &Path, _spec: &SplitSpec, _out_dir: &Path) -> Result<Vec<PathBuf>> {
    todo!("stage 1: split")
}
