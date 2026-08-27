//! Page selection.
//!
//! Users type one-indexed, inclusive ranges (`1-3,7,9-`). Everything inside the
//! library is zero-indexed. The conversion happens here and nowhere else.

use crate::error::{PdfError, Result};

/// A parsed page selection, kept in the form the user wrote it so that it can be
/// resolved against documents of different lengths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageRange {
    /// Every page in the document.
    All,
    /// An ordered list of segments, evaluated left to right.
    Segments(Vec<Segment>),
}

/// One comma-separated piece of a page range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    /// A single page, one-indexed.
    Single(usize),
    /// An inclusive span, one-indexed.
    Span { start: usize, end: usize },
    /// `N-`: from page N to the end of the document.
    From(usize),
}

impl PageRange {
    /// Parse a user-supplied range such as `1-3,7,9-` or `all`.
    ///
    /// Stage 1 replaces this with the real implementation and its unit tests.
    pub fn parse(_spec: &str) -> Result<Self> {
        todo!("stage 1: page range parsing")
    }

    /// Resolve to zero-indexed page positions against a document of
    /// `page_count` pages, preserving the order the user wrote and rejecting
    /// out-of-range pages.
    ///
    /// Stage 1 replaces this with the real implementation.
    pub fn resolve(&self, _page_count: usize) -> Result<Vec<usize>> {
        todo!("stage 1: page range resolution")
    }
}

/// Convert a one-indexed page number to a zero-indexed position, bounds-checked
/// against `total`.
///
/// This is the single crossing point between what the user types and what the
/// library indexes with; every other module should go through it.
pub fn to_zero_indexed(requested: usize, total: usize) -> Result<usize> {
    if requested == 0 || requested > total {
        return Err(PdfError::PageOutOfRange { requested, total });
    }
    Ok(requested - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_indexing_is_bounds_checked() {
        assert_eq!(to_zero_indexed(1, 3).unwrap(), 0);
        assert_eq!(to_zero_indexed(3, 3).unwrap(), 2);
        assert!(to_zero_indexed(0, 3).is_err());
        assert!(to_zero_indexed(4, 3).is_err());
    }
}
