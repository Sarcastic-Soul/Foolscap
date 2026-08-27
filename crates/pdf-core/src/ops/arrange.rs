//! Reordering and removing pages.
//!
//! Both are the same operation as extraction, expressed differently: choose a
//! sequence of the document's pages and rebuild around it. They go through
//! [`assemble`](crate::assemble) like every other page-tree rebuild, so
//! inherited attributes and repeated pages behave the same way here as in a
//! merge or a split.

use crate::assemble::{assemble, Source};
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;

/// Rebuild `doc` with its pages in the given order.
///
/// `order` holds zero-indexed positions. Pages left out are dropped, and a
/// page named twice appears twice, as its own object.
pub fn arrange(doc: Document, order: &[usize]) -> Result<Document> {
    if order.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    let Document { inner, source } = doc;

    let assembled = assemble(vec![Source {
        doc: inner,
        pages: order.to_vec(),
    }])?;

    Ok(Document::from_lopdf(assembled, source))
}

/// Remove the selected pages, keeping the rest in their existing order.
pub fn delete(doc: Document, pages: &PageRange) -> Result<Document> {
    let page_count = doc.page_count();
    let doomed = pages.resolve_unique(page_count)?;

    let keep: Vec<usize> = (0..page_count)
        .filter(|position| !doomed.contains(position))
        .collect();

    if keep.is_empty() {
        // A PDF with no pages is not a document anyone can open, so refuse
        // rather than writing something broken.
        return Err(PdfError::EmptySelection);
    }

    arrange(doc, &keep)
}

/// Move the selected pages so that they sit immediately before position
/// `before`, keeping their relative order.
///
/// `before` is a zero-indexed position in the *original* document, and may
/// equal the page count, meaning "at the end".
pub fn move_pages(doc: Document, pages: &PageRange, before: usize) -> Result<Document> {
    let page_count = doc.page_count();
    let moving = pages.resolve_unique(page_count)?;

    if moving.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    if before > page_count {
        return Err(PdfError::PageOutOfRange {
            requested: before + 1,
            total: page_count,
        });
    }

    let mut order = Vec::with_capacity(page_count);
    for position in 0..=page_count {
        // Insert the moved run at the target position, before whatever was
        // there. Doing this on the way past keeps one pass.
        if position == before {
            order.extend(moving.iter().copied());
        }
        if position < page_count && !moving.contains(&position) {
            order.push(position);
        }
    }

    arrange(doc, &order)
}

#[cfg(test)]
mod tests {
    /// The ordering logic alone, without needing a real document.
    fn moved(page_count: usize, moving: &[usize], before: usize) -> Vec<usize> {
        let mut order = Vec::with_capacity(page_count);
        for position in 0..=page_count {
            if position == before {
                order.extend(moving.iter().copied());
            }
            if position < page_count && !moving.contains(&position) {
                order.push(position);
            }
        }
        order
    }

    #[test]
    fn moving_a_page_forward_puts_it_before_the_target() {
        assert_eq!(moved(5, &[0], 3), vec![1, 2, 0, 3, 4]);
    }

    #[test]
    fn moving_a_page_backward_puts_it_before_the_target() {
        assert_eq!(moved(5, &[4], 1), vec![0, 4, 1, 2, 3]);
    }

    #[test]
    fn moving_to_the_end_is_allowed() {
        assert_eq!(moved(4, &[0], 4), vec![1, 2, 3, 0]);
    }

    #[test]
    fn moving_to_the_start_is_allowed() {
        assert_eq!(moved(4, &[3], 0), vec![3, 0, 1, 2]);
    }

    #[test]
    fn a_moved_run_keeps_its_relative_order() {
        assert_eq!(moved(6, &[1, 3], 5), vec![0, 2, 4, 1, 3, 5]);
    }

    #[test]
    fn moving_a_page_to_where_it_already_is_changes_nothing() {
        assert_eq!(moved(4, &[2], 2), vec![0, 1, 2, 3]);
    }

    #[test]
    fn every_page_survives_a_move() {
        let order = moved(7, &[2, 5], 1);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..7).collect::<Vec<_>>());
    }
}
