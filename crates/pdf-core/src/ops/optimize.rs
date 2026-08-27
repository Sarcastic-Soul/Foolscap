use crate::document::Document;
use crate::error::Result;

/// How aggressively to clean up the document.
///
/// Every level here is lossless. Image downsampling is a separate, lossy
/// operation that arrives in stage 3 as `compress`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OptimizeLevel {
    /// Drop objects unreachable from the document catalog.
    #[default]
    Safe,
    /// Also deduplicate identical streams and re-encode content streams.
    Aggressive,
}

/// What an optimize pass changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptimizeReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub objects_removed: usize,
    pub streams_deduplicated: usize,
}

impl OptimizeReport {
    /// Bytes saved, saturating at zero if the pass made the file larger.
    pub fn bytes_saved(&self) -> u64 {
        self.bytes_before.saturating_sub(self.bytes_after)
    }
}

/// Losslessly shrink `doc`.
pub fn optimize(_doc: &mut Document, _level: OptimizeLevel) -> Result<OptimizeReport> {
    todo!("stage 1: optimize")
}
