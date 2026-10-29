//! Cut each page in half, for scanned double-page spreads.
//!
//! Every page becomes two: the effective box (its `/CropBox` inside its
//! `/MediaBox`) is split down the middle — left and right halves for a
//! vertical cut, top and bottom for a horizontal one — with `gap` points
//! dropped at the cut line so a gutter or a scanning shadow can be removed.
//! The halves are new crop boxes on copies of the page, so the drawn content
//! is untouched and `reset_crop` can undo the operation.
//!
//! The page tree is rebuilt through [`assemble`](crate::assemble), the same as
//! every other reassembly, so inherited attributes and repeated pages behave
//! here as they do in a merge or a split.

use lopdf::{Object, ObjectId};

use crate::assemble::{assemble, materialize_inherited_attributes, Source};
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::ops::crop::Rect;

/// Which way the cut runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalveDirection {
    /// A vertical cut, producing left and right halves.
    Vertical,
    /// A horizontal cut, producing top and bottom halves.
    Horizontal,
}

/// Options for [`halve`].
#[derive(Debug, Clone)]
pub struct HalveOptions {
    pub direction: HalveDirection,
    /// Extra margin to drop at the cut line, in points, for a gutter.
    pub gap: f64,
    /// Emit the halves in swapped order.
    pub swap: bool,
}

/// The two halves of one page's visible box, in reading order.
#[derive(Debug)]
struct Halves {
    first: Rect,
    second: Rect,
}

/// Replace every page with its two halves, in reading order.
///
/// A vertical cut reads left then right; a horizontal cut reads top then
/// bottom. `swap` reverses each pair, for spreads scanned right-to-left. A
/// `gap` that would leave either half with nothing is
/// [`PdfError::InvalidCropBox`], naming the page.
pub fn halve(doc: &mut Document, options: &HalveOptions) -> Result<()> {
    if !options.gap.is_finite() || options.gap < 0.0 {
        return Err(PdfError::InvalidCropBox {
            reason: format!("the gap must be zero or more, got {}", options.gap),
        });
    }
    let page_count = doc.page_count();
    if page_count == 0 {
        return Err(PdfError::EmptySelection);
    }

    // Pages may inherit /MediaBox and /CropBox from the page tree; materialise
    // them first so every half is cut from its page's own effective box.
    materialize_inherited_attributes(&mut doc.inner)?;
    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();

    // Work out every page's halves before rebuilding anything, so a page the
    // gap would destroy leaves the document untouched.
    let mut halves: Vec<Halves> = Vec::with_capacity(page_count);
    for (position, page_id) in page_ids.iter().enumerate() {
        halves.push(split_box(
            effective_box(&doc.inner, *page_id, position)?,
            options,
            position,
        )?);
    }

    // Two copies of each page through the shared reassembly. The output order
    // matches the order given, so output positions 2i and 2i+1 are source
    // page i's copies.
    let mut order: Vec<usize> = Vec::with_capacity(page_count * 2);
    for position in 0..page_count {
        order.push(position);
        order.push(position);
    }

    let inner = std::mem::replace(&mut doc.inner, lopdf::Document::new());
    let was_encrypted = doc.was_encrypted;
    let source = doc.source.take();
    let mut assembled = assemble(vec![Source {
        doc: inner,
        pages: order,
    }])?;

    let out_ids: Vec<ObjectId> = assembled.get_pages().into_values().collect();
    for (position, pair) in halves.iter().enumerate() {
        let (first, second) = if options.swap {
            (pair.second, pair.first)
        } else {
            (pair.first, pair.second)
        };
        for (copy, rect) in [(0usize, first), (1, second)] {
            let page = assembled
                .get_object_mut(out_ids[position * 2 + copy])?
                .as_dict_mut()?;
            page.set("CropBox", box_array(rect));
        }
    }

    let mut halved = Document::from_lopdf(assembled, source);
    halved.was_encrypted = was_encrypted;
    *doc = halved;

    Ok(())
}

/// The box a page shows now: its `/CropBox` where it has one that overlaps the
/// media box, otherwise the `/MediaBox` itself.
fn effective_box(doc: &lopdf::Document, page_id: ObjectId, position: usize) -> Result<Rect> {
    let media = read_box(doc, page_id, b"MediaBox")?.ok_or_else(|| PdfError::InvalidMediaBox {
        reason: format!("page {} has no /MediaBox", position + 1),
    })?;
    let visible = match read_box(doc, page_id, b"CropBox")? {
        Some(crop) => intersect(&crop, &media).unwrap_or(media),
        None => media,
    };
    if visible.width() <= 0.0 || visible.height() <= 0.0 {
        return Err(PdfError::InvalidMediaBox {
            reason: format!("page {} has a degenerate visible box", position + 1),
        });
    }
    Ok(visible)
}

/// Cut `visible` in half, shrinking both halves by the gap at the cut line.
fn split_box(visible: Rect, options: &HalveOptions, position: usize) -> Result<Halves> {
    let gap = options.gap as f32;
    let (first, second) = match options.direction {
        HalveDirection::Vertical => {
            let mid = (visible.x0 + visible.x1) / 2.0;
            (
                Rect {
                    x1: mid - gap,
                    ..visible
                },
                Rect {
                    x0: mid + gap,
                    ..visible
                },
            )
        }
        HalveDirection::Horizontal => {
            let mid = (visible.y0 + visible.y1) / 2.0;
            (
                Rect {
                    y0: mid + gap,
                    ..visible
                }, // top half
                Rect {
                    y1: mid - gap,
                    ..visible
                }, // bottom half
            )
        }
    };
    if first.width() <= 0.0
        || first.height() <= 0.0
        || second.width() <= 0.0
        || second.height() <= 0.0
    {
        return Err(PdfError::InvalidCropBox {
            reason: format!(
                "the gap of {gap} points leaves nothing of page {}'s halves",
                position + 1
            ),
        });
    }
    Ok(Halves { first, second })
}

/// Read a `/MediaBox` or `/CropBox` entry from a page, accepting integers and
/// reals, indirect or direct.
fn read_box(doc: &lopdf::Document, page_id: ObjectId, key: &[u8]) -> Result<Option<Rect>> {
    let Ok(dict) = doc.get_dictionary(page_id) else {
        return Ok(None);
    };
    let Ok(object) = dict.get(key) else {
        return Ok(None);
    };
    let resolved = doc
        .dereference(object)
        .map(|(_, object)| object.clone())
        .unwrap_or_else(|_| object.clone());

    let Object::Array(values) = resolved else {
        return Err(PdfError::InvalidMediaBox {
            reason: format!(
                "{} is not an array of four numbers",
                String::from_utf8_lossy(key)
            ),
        });
    };
    if values.len() != 4 {
        return Err(PdfError::InvalidMediaBox {
            reason: format!(
                "{} has {} entries, expected 4",
                String::from_utf8_lossy(key),
                values.len()
            ),
        });
    }
    let mut numbers = [0.0f32; 4];
    for (index, value) in values.iter().enumerate() {
        numbers[index] = match value {
            Object::Integer(number) => *number as f32,
            Object::Real(number) => *number,
            _ => {
                return Err(PdfError::InvalidMediaBox {
                    reason: format!("{} entries must be numbers", String::from_utf8_lossy(key)),
                })
            }
        };
    }
    if !numbers.iter().all(|number| number.is_finite()) {
        return Err(PdfError::InvalidMediaBox {
            reason: format!("{} entries must be finite", String::from_utf8_lossy(key)),
        });
    }
    Ok(Some(Rect {
        x0: numbers[0],
        y0: numbers[1],
        x1: numbers[2],
        y1: numbers[3],
    }))
}

/// The overlap of two rectangles, or `None` when they do not touch.
fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let rect = Rect {
        x0: a.x0.max(b.x0),
        y0: a.y0.max(b.y0),
        x1: a.x1.min(b.x1),
        y1: a.y1.min(b.y1),
    };
    (rect.x1 > rect.x0 && rect.y1 > rect.y0).then_some(rect)
}

fn box_array(rect: Rect) -> Object {
    Object::Array(vec![
        Object::Real(rect.x0),
        Object::Real(rect.y0),
        Object::Real(rect.x1),
        Object::Real(rect.y1),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vertical_cut_shrinks_both_halves_at_the_line() {
        let page = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 842.0,
            y1: 595.0,
        };
        let halves = split_box(
            page,
            &HalveOptions {
                direction: HalveDirection::Vertical,
                gap: 0.0,
                swap: false,
            },
            0,
        )
        .unwrap();
        assert_eq!(halves.first.x1, 421.0);
        assert_eq!(halves.second.x0, 421.0);

        let gapped = split_box(
            page,
            &HalveOptions {
                direction: HalveDirection::Vertical,
                gap: 12.0,
                swap: false,
            },
            0,
        )
        .unwrap();
        assert_eq!(gapped.first.x1, 409.0);
        assert_eq!(gapped.second.x0, 433.0);
    }

    #[test]
    fn a_horizontal_cut_reads_top_then_bottom() {
        let page = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 595.0,
            y1: 842.0,
        };
        let halves = split_box(
            page,
            &HalveOptions {
                direction: HalveDirection::Horizontal,
                gap: 0.0,
                swap: false,
            },
            0,
        )
        .unwrap();
        assert_eq!(halves.first.y0, 421.0, "the top half comes first");
        assert_eq!(halves.second.y1, 421.0);
    }

    #[test]
    fn a_gap_that_eats_a_half_is_refused() {
        let page = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        };
        let error = split_box(
            page,
            &HalveOptions {
                direction: HalveDirection::Vertical,
                gap: 60.0,
                swap: false,
            },
            3,
        )
        .unwrap_err();
        assert!(matches!(error, PdfError::InvalidCropBox { .. }));
    }

    #[test]
    fn a_negative_gap_is_refused_up_front() {
        let mut doc = Document::from_lopdf(lopdf::Document::new(), None);
        let error = halve(
            &mut doc,
            &HalveOptions {
                direction: HalveDirection::Vertical,
                gap: -1.0,
                swap: false,
            },
        )
        .unwrap_err();
        assert!(matches!(error, PdfError::InvalidCropBox { .. }));
    }

    #[test]
    fn intersection_clamps_a_crop_box_to_the_media_box() {
        let media = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        };
        let overlapping = Rect {
            x0: 50.0,
            y0: 50.0,
            x1: 150.0,
            y1: 150.0,
        };
        let clamped = intersect(&overlapping, &media).unwrap();
        assert_eq!(
            (clamped.x0, clamped.y0, clamped.x1, clamped.y1),
            (50.0, 50.0, 100.0, 100.0)
        );

        let away = Rect {
            x0: 200.0,
            y0: 200.0,
            x1: 300.0,
            y1: 300.0,
        };
        assert!(intersect(&away, &media).is_none());
    }
}
