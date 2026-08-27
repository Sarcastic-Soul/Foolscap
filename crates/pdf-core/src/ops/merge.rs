use std::path::{Path, PathBuf};

use crate::assemble::{assemble, Source};
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::progress::{Progress, ProgressFn};

/// Concatenate `inputs` in order into a single document at `output`.
///
/// The result has exactly the sum of the inputs' page counts.
pub fn merge(inputs: &[PathBuf], output: &Path) -> Result<()> {
    merge_with_progress(inputs, output, None)
}

/// [`merge`], reporting one progress tick per input document read.
pub fn merge_with_progress(
    inputs: &[PathBuf],
    output: &Path,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    if inputs.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    let total = inputs.len();
    let mut sources = Vec::with_capacity(total);

    for (index, path) in inputs.iter().enumerate() {
        if let Some(report) = progress.as_mut() {
            report(Progress::new(
                index,
                Some(total),
                format!("reading {}", path.display()),
            ));
        }

        let doc = Document::open(path)?;
        tracing::debug!(path = %path.display(), pages = doc.page_count(), "merging input");
        sources.push(Source::all(doc.inner));
    }

    if let Some(report) = progress.as_mut() {
        report(Progress::new(total, Some(total), "writing output"));
    }

    let merged = assemble(sources)?;
    Document::from_lopdf(merged, None).save(output)
}
