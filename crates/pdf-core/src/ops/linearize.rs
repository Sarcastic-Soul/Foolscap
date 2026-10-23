use lopdf::{Object, SaveOptions};

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::progress::{Progress, ProgressFn};

/// Linearize a PDF document for fast web view (byte-serving).
///
/// This reorders the PDF's internal structure so that:
/// - The cross-reference table and linearization dictionary come first
/// - Page 1 objects appear early in the file
/// - The document can be served over HTTP range requests without downloading the whole file
///
/// The operation is lossless and does not change the visual appearance of the document.
pub fn linearize(doc: &mut Document) -> Result<()> {
    let mut progress_fn: ProgressFn = &mut |_| {};
    linearize_with_progress(doc, progress_fn)
}

/// Linearize a PDF document with progress reporting.
pub fn linearize_with_progress(doc: &mut Document, mut progress: ProgressFn) -> Result<()> {
    progress(Progress {
        current: 0,
        total: Some(1),
        message: "Linearizing document".into(),
    });

    // Use lopdf's SaveOptions with linearize enabled
    let options = SaveOptions {
        linearize: true,
        use_object_streams: true,
        use_xref_streams: true,
        object_stream_config: Default::default(),
    };

    // Save to a buffer with linearization options, then reload
    // This is necessary because linearization reorganizes the entire file structure
    let mut buffer = Vec::new();
    doc.inner
        .save_with_options(&mut buffer, options)
        .map_err(|source| PdfError::Internal(format!("linearization failed: {source}")))?;

    // Reload the linearized document
    doc.inner = lopdf::Document::load_mem(&buffer)
        .map_err(|source| PdfError::Internal(format!("failed to reload linearized document: {source}")))?;

    progress(Progress {
        current: 1,
        total: Some(1),
        message: "Linearization complete".into(),
    });

    tracing::debug!("document linearized for fast web view");
    Ok(())
}

/// Check if a document is already linearized.
pub fn is_linearized(doc: &Document) -> bool {
    // In a linearized PDF, there should be a linearization dictionary
    // with a /Linearized entry. We check all objects for this marker.
    for obj in doc.inner.objects.values() {
        match obj {
            Object::Dictionary(dict) => {
                if dict.has(b"Linearized") {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn test_linearize_empty_doc() {
        let mut doc = Document::from_lopdf(lopdf::Document::new(), None);
        linearize(&mut doc).unwrap();
        // Should not panic
    }
}