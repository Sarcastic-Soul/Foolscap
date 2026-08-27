use std::path::{Path, PathBuf};

use crate::assemble::{assemble, Source};
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

/// How to divide a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitSpec {
    /// Extract the selected pages into one output file, in the order given.
    Extract(PageRange),
    /// Burst into consecutive chunks of `n` pages each. The final chunk may be
    /// shorter.
    Every(usize),
}

/// Split `input` according to `spec`, writing results into `out_dir`.
///
/// Returns the paths written, in order. Output files are named after the input
/// stem: `report-extract.pdf` for an extraction, `report-001.pdf` and onwards
/// for a burst.
pub fn split(input: &Path, spec: &SplitSpec, out_dir: &Path) -> Result<Vec<PathBuf>> {
    split_with_progress(input, spec, out_dir, None)
}

/// The paths [`split`] would write, without writing anything.
///
/// Callers that want to ask before overwriting need the names in advance;
/// deriving them here rather than in the caller keeps one naming scheme.
pub fn plan(input: &Path, spec: &SplitSpec, out_dir: &Path) -> Result<Vec<PathBuf>> {
    let page_count = Document::open(input)?.page_count();
    let chunks = chunks(spec, page_count)?;
    Ok(output_paths(input, spec, out_dir, chunks.len()))
}

/// [`split`], reporting one progress tick per output file written.
pub fn split_with_progress(
    input: &Path,
    spec: &SplitSpec,
    out_dir: &Path,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<Vec<PathBuf>> {
    let page_count = Document::open(input)?.page_count();
    let chunks = chunks(spec, page_count)?;

    let total = chunks.len();
    let paths = output_paths(input, spec, out_dir, total);
    let mut written = Vec::with_capacity(total);

    for (index, (pages, path)) in chunks.into_iter().zip(paths).enumerate() {
        if let Some(report) = progress.as_mut() {
            report(Progress::new(
                index,
                Some(total),
                format!("writing {}", path.display()),
            ));
        }

        // Each output needs its own copy of the source: assembly consumes the
        // document it is given.
        let source_doc = Document::open(input)?;
        let assembled = assemble(vec![Source {
            doc: source_doc.inner,
            pages,
        }])?;
        Document::from_lopdf(assembled, None).save(&path)?;
        written.push(path);
    }

    if let Some(report) = progress.as_mut() {
        report(Progress::new(total, Some(total), "done"));
    }

    Ok(written)
}

/// Group the document's pages into one selection per output file.
fn chunks(spec: &SplitSpec, page_count: usize) -> Result<Vec<Vec<usize>>> {
    let chunks: Vec<Vec<usize>> = match spec {
        SplitSpec::Extract(range) => vec![range.resolve(page_count)?],
        SplitSpec::Every(size) => {
            if *size == 0 {
                return Err(PdfError::InvalidPageRange {
                    spec: "--every 0".into(),
                    reason: "chunk size must be at least 1".into(),
                });
            }
            (0..page_count)
                .collect::<Vec<_>>()
                .chunks(*size)
                .map(<[usize]>::to_vec)
                .collect()
        }
    };

    if chunks.iter().all(Vec::is_empty) {
        return Err(PdfError::EmptySelection);
    }

    Ok(chunks)
}

/// Derive the output file names for a split of `count` pieces.
fn output_paths(input: &Path, spec: &SplitSpec, out_dir: &Path, count: usize) -> Vec<PathBuf> {
    let stem = file_stem(input);
    let width = digits(count);

    (0..count)
        .map(|index| {
            let name = match spec {
                SplitSpec::Extract(_) => format!("{stem}-extract.pdf"),
                SplitSpec::Every(_) => format!("{stem}-{:0width$}.pdf", index + 1, width = width),
            };
            out_dir.join(name)
        })
        .collect()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_string())
}

fn digits(value: usize) -> usize {
    value.max(1).to_string().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names_are_zero_padded_to_the_chunk_count() {
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(100), 3);
        assert_eq!(digits(0), 1);
    }

    #[test]
    fn stem_falls_back_when_the_path_has_none() {
        assert_eq!(file_stem(Path::new("report.pdf")), "report");
        assert_eq!(file_stem(Path::new("/tmp/a.b.pdf")), "a.b");
        assert_eq!(file_stem(Path::new("/")), "document");
    }
}
