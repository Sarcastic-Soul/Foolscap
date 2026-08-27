use std::collections::HashMap;

use lopdf::{Object, ObjectId};

use crate::document::Document;
use crate::error::{PdfError, Result};

/// How aggressively to clean up the document.
///
/// Every level here is lossless. Image downsampling is a separate, lossy
/// operation that arrives in stage 3 as `compress`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OptimizeLevel {
    /// Drop objects unreachable from the trailer, and zero-length streams.
    #[default]
    Safe,
    /// Also deduplicate byte-identical streams and Flate-compress the result.
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

    /// Proportion of the original size removed, in the range `0.0..=1.0`.
    pub fn ratio_saved(&self) -> f64 {
        if self.bytes_before == 0 {
            return 0.0;
        }
        self.bytes_saved() as f64 / self.bytes_before as f64
    }
}

/// Losslessly shrink `doc`.
///
/// Sizes in the report are measured by serialising the document before and
/// after, so they describe what will actually be written rather than the size
/// of whatever file the document was loaded from.
pub fn optimize(doc: &mut Document, level: OptimizeLevel) -> Result<OptimizeReport> {
    let bytes_before = serialized_len(doc)?;

    let mut report = OptimizeReport {
        bytes_before,
        ..Default::default()
    };

    report.objects_removed += doc.inner.delete_zero_length_streams().len();

    if level == OptimizeLevel::Aggressive {
        report.streams_deduplicated = deduplicate_streams(&mut doc.inner);
        doc.inner.compress();
    }

    report.objects_removed += doc.inner.prune_objects().len();
    doc.inner.renumber_objects();

    report.bytes_after = serialized_len(doc)?;

    tracing::debug!(
        before = report.bytes_before,
        after = report.bytes_after,
        removed = report.objects_removed,
        deduplicated = report.streams_deduplicated,
        "optimize pass complete"
    );

    Ok(report)
}

/// Point every reference to a byte-identical stream at a single copy.
///
/// Returns the number of duplicate objects retired. The duplicates themselves
/// are left for [`lopdf::Document::prune_objects`] to collect once nothing
/// refers to them.
fn deduplicate_streams(doc: &mut lopdf::Document) -> usize {
    // Key on the stream's dictionary and its bytes together: two streams with
    // the same content but different filters or decode parameters are not
    // interchangeable.
    let mut canonical: HashMap<(String, Vec<u8>), ObjectId> = HashMap::new();
    let mut replacements: HashMap<ObjectId, ObjectId> = HashMap::new();

    for (id, object) in &doc.objects {
        let Object::Stream(stream) = object else {
            continue;
        };

        let key = (format!("{:?}", stream.dict), stream.content.clone());
        match canonical.get(&key) {
            Some(first) => {
                replacements.insert(*id, *first);
            }
            None => {
                canonical.insert(key, *id);
            }
        }
    }

    if replacements.is_empty() {
        return 0;
    }

    doc.traverse_objects(|object| {
        if let Object::Reference(id) = object {
            if let Some(canonical) = replacements.get(id) {
                *object = Object::Reference(*canonical);
            }
        }
    });

    replacements.len()
}

/// Serialise the document to memory and report its length.
///
/// Measures a clone: writing a document appends a cross-reference stream object
/// to it, so serialising the real document here would change what a later save
/// produces and make the reported size wrong by a few bytes.
fn serialized_len(doc: &Document) -> Result<u64> {
    let mut buffer = Vec::new();
    doc.inner
        .clone()
        .save_to(&mut buffer)
        .map_err(|source| PdfError::Internal(format!("could not serialise document: {source}")))?;
    Ok(buffer.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_never_reports_a_negative_saving() {
        let report = OptimizeReport {
            bytes_before: 100,
            bytes_after: 120,
            ..Default::default()
        };
        assert_eq!(report.bytes_saved(), 0);
        assert_eq!(report.ratio_saved(), 0.0);
    }

    #[test]
    fn ratio_is_a_fraction_of_the_original() {
        let report = OptimizeReport {
            bytes_before: 1000,
            bytes_after: 250,
            ..Default::default()
        };
        assert_eq!(report.bytes_saved(), 750);
        assert!((report.ratio_saved() - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn an_empty_document_has_no_ratio() {
        assert_eq!(OptimizeReport::default().ratio_saved(), 0.0);
    }
}
