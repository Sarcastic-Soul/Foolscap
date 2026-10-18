//! Redaction, first version: opaque boxes over rectangles or found text.
//!
//! `redact` draws an opaque filled rectangle (`re f`) over the area and
//! removes text-showing operations so the covered words are no longer
//! searchable or copyable. Two deliberate v1 limits:
//!
//! - Rectangles do not know which glyphs they overlap: without a layout pass,
//!   every `Tj`/`TJ` run on a redacted page is removed, not just the ones
//!   under the box. The page keeps its vectors and images; it loses its
//!   embedded text runs. Coarse, but the box hides the pixels and the removal
//!   strips the text from search.
//! - `redact_find` removes the runs whose decoded string contains the needle,
//!   wherever they sit, and draws no box: with no glyph coordinates there is
//!   nowhere honest to put one.
//!
//! - Only the page's own content stream is searched. Text inside Form XObjects
//!   and annotations, and any image pixels under a box, are left as they are.
//!
//! A later version should position glyphs and remove only the runs a box
//! covers.

use lopdf::content::{Content, Operation};
use lopdf::{Object, ObjectId};

use super::crop::Rect;
use crate::assemble::materialize_inherited_attributes;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

/// What a redaction pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactReport {
    /// Pages the pass touched.
    pub pages_redacted: usize,
    /// Text-showing runs removed from the content streams.
    pub runs_removed: usize,
    /// Opaque boxes drawn.
    pub boxes_drawn: usize,
}

/// Cover `rect` on every selected page with an opaque black box and strip the
/// page's text runs so the covered words leave no searchable trace.
///
/// See the module notes for why every run on the page goes rather than only
/// the overlapping ones.
pub fn redact(doc: &mut Document, pages: &PageRange, rect: Rect) -> Result<RedactReport> {
    redact_with_progress(doc, pages, rect, None)
}

/// [`redact`], reporting one progress tick per selected page.
pub fn redact_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    rect: Rect,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<RedactReport> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(PdfError::InvalidCropBox {
            reason: "the redaction box must have a positive width and height".into(),
        });
    }

    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    materialize_inherited_attributes(&mut doc.inner)?;
    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();
    let mut report = RedactReport::default();

    for (index, position) in selected.iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(selected.len()),
                format!("redacting page {}", position + 1),
            ));
        }
        let removed = strip_text_runs(&mut doc.inner, page_ids[*position], &|_| true)?;
        append_box(&mut doc.inner, page_ids[*position], rect)?;
        report.pages_redacted += 1;
        report.runs_removed += removed;
        report.boxes_drawn += 1;
    }

    Ok(report)
}

/// Remove the text runs containing `needle` on the selected pages.
///
/// Matching is a case-sensitive substring over each run's decoded bytes. The
/// runs are removed from the content stream, which strips them from search and
/// copying; no box is drawn, since v1 has no glyph coordinates to place one
/// honestly.
pub fn redact_find(doc: &mut Document, pages: &PageRange, needle: &str) -> Result<RedactReport> {
    redact_find_with_progress(doc, pages, needle, None)
}

/// [`redact_find`], reporting one progress tick per selected page.
pub fn redact_find_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    needle: &str,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<RedactReport> {
    if needle.is_empty() {
        return Err(PdfError::EmptySearchText);
    }

    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    materialize_inherited_attributes(&mut doc.inner)?;
    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();
    let mut report = RedactReport::default();

    for (index, position) in selected.iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(selected.len()),
                format!("searching page {}", position + 1),
            ));
        }
        let removed = strip_text_runs(&mut doc.inner, page_ids[*position], &|text| {
            text.contains(needle)
        })?;
        if removed > 0 {
            report.pages_redacted += 1;
            report.runs_removed += removed;
        }
    }

    if report.runs_removed == 0 {
        return Err(PdfError::TextNotFound(needle.to_string()));
    }

    Ok(report)
}

/// Remove the text-showing operations for which `matches` holds, keeping the
/// rest.
///
/// Returns the number of runs removed. Text-showing operators are `Tj`, `TJ`,
/// `'` and `"`.
fn strip_text_runs(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    matches: &dyn Fn(&str) -> bool,
) -> Result<usize> {
    let content = doc.inner_content(page_id);
    let decoded = Content::decode(&content).map_err(|source| {
        PdfError::Internal(format!("could not decode the page content: {source}"))
    })?;

    let mut removed = 0;
    let mut kept = Vec::with_capacity(decoded.operations.len());
    for operation in decoded.operations {
        if is_text_showing(&operation) && matches(&operation_text(&operation)) {
            removed += 1;
            continue;
        }
        kept.push(operation);
    }

    if removed > 0 {
        let encoded = Content { operations: kept }.encode().map_err(|source| {
            PdfError::Internal(format!("could not encode the page content: {source}"))
        })?;
        doc.change_page_content(page_id, encoded)?;
    }

    Ok(removed)
}

fn is_text_showing(operation: &Operation) -> bool {
    matches!(operation.operator.as_str(), "Tj" | "TJ" | "'" | "\"")
}

/// The searchable text an operation draws, decoded lossily from its string
/// operands. Enough for a coarse v1 match, not a text extractor.
fn operation_text(operation: &Operation) -> String {
    fn bytes_to_text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    match operation.operator.as_str() {
        "Tj" | "'" => operation
            .operands
            .first()
            .map(|operand| match operand {
                Object::String(bytes, _) => bytes_to_text(bytes),
                _ => String::new(),
            })
            .unwrap_or_default(),
        "\"" => operation
            .operands
            .last()
            .map(|operand| match operand {
                Object::String(bytes, _) => bytes_to_text(bytes),
                _ => String::new(),
            })
            .unwrap_or_default(),
        "TJ" => operation
            .operands
            .first()
            .and_then(|operand| operand.as_array().ok())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match item {
                        Object::String(bytes, _) => Some(bytes_to_text(bytes)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Append an opaque black `re f` for `rect`, isolated in `q ... Q`.
fn append_box(doc: &mut lopdf::Document, page_id: ObjectId, rect: Rect) -> Result<()> {
    let content = doc.inner_content(page_id);
    let mut decoded = Content::decode(&content).map_err(|source| {
        PdfError::Internal(format!("could not decode the page content: {source}"))
    })?;

    decoded.operations.push(Operation::new("q", vec![]));
    // Filling in device grey keeps one line and no resource lookup.
    decoded
        .operations
        .push(Operation::new("g", vec![Object::Integer(0)]));
    decoded.operations.push(Operation::new(
        "re",
        vec![
            Object::Real(rect.x0),
            Object::Real(rect.y0),
            Object::Real(rect.width()),
            Object::Real(rect.height()),
        ],
    ));
    decoded.operations.push(Operation::new("f", vec![]));
    decoded.operations.push(Operation::new("Q", vec![]));

    let encoded = Content {
        operations: decoded.operations,
    }
    .encode()
    .map_err(|source| PdfError::Internal(format!("could not encode the page content: {source}")))?;
    doc.change_page_content(page_id, encoded)?;

    Ok(())
}

/// [`lopdf::Document::get_page_content`] on a borrowed handle: the inherent
/// method takes `&self`, so this is only spelled out to keep the call sites
/// short.
trait PageContent {
    fn inner_content(&self, page_id: ObjectId) -> Vec<u8>;
}

impl PageContent for lopdf::Document {
    fn inner_content(&self, page_id: ObjectId) -> Vec<u8> {
        self.get_page_content(page_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_op(operator: &str, text: &str) -> Operation {
        Operation::new(
            operator,
            vec![Object::string_literal(text.as_bytes().to_vec())],
        )
    }

    #[test]
    fn text_showing_operators_are_recognised() {
        for operator in ["Tj", "TJ", "'", "\""] {
            assert!(is_text_showing(&Operation::new(operator, vec![])));
        }
        assert!(!is_text_showing(&Operation::new("cm", vec![])));
    }

    #[test]
    fn tj_text_is_extracted() {
        assert_eq!(operation_text(&text_op("Tj", "hello")), "hello");
    }

    #[test]
    fn tj_array_text_is_joined() {
        let operation = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(b"he".to_vec()),
                Object::Integer(-40),
                Object::string_literal(b"llo".to_vec()),
            ])],
        );
        assert_eq!(operation_text(&operation), "hello");
    }
}
