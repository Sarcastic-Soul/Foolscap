//! PDF to SVG conversion. Needs the `render` feature.

use std::path::{Path, PathBuf};

use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

#[cfg(feature = "render")]
use mupdf::{Document as MuPdfDocument, Matrix};

/// How to handle fonts in the SVG output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontEmbedding {
    /// Reference fonts by name only (smallest file, requires fonts on viewer system).
    Reference,
    /// Subset and embed only the glyphs used (recommended default).
    #[default]
    Subset,
    /// Embed full font files (largest file, maximum portability).
    None,
}

/// Options for [`to_svg`].
#[derive(Debug, Clone, PartialEq)]
pub struct SvgOptions {
    /// Page range to convert.
    pub pages: PageRange,
    /// Font embedding strategy.
    pub font_embedding: FontEmbedding,
    /// Convert text to vector paths instead of keeping it as selectable text.
    pub text_as_paths: bool,
    /// Password for an encrypted input, used only if the file needs one.
    pub password: Option<String>,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            pages: PageRange::All,
            font_embedding: FontEmbedding::Subset,
            text_as_paths: false,
            password: None,
        }
    }
}

/// The paths [`to_svg`] would write, without writing anything.
///
/// The CLI needs these in advance to ask before overwriting.
#[cfg(feature = "render")]
pub fn plan(input: &Path, out_dir: &Path, options: &SvgOptions) -> Result<Vec<PathBuf>> {
    let doc = open_mupdf_document(input, options.password.as_deref())?;
    let page_count = doc
        .page_count()
        .map_err(|source| mupdf_error_from_doc(&doc, source))?;
    let selected = options.pages.resolve(page_count as usize)?;
    Ok(output_paths(input, out_dir, &selected, page_count))
}

/// Convert the selected pages of a PDF to SVG files, one per page.
///
/// Each page is written as `{stem}-{page}.svg` in `out_dir`, with page numbers
/// one-indexed and zero-padded to the total page count.
#[cfg(feature = "render")]
pub fn to_svg(input: &Path, out_dir: &Path, options: &SvgOptions) -> Result<Vec<PathBuf>> {
    to_svg_with_progress(input, out_dir, options, None)
}

/// [`to_svg`], reporting one progress tick per page.
#[cfg(feature = "render")]
pub fn to_svg_with_progress(
    input: &Path,
    out_dir: &Path,
    options: &SvgOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<Vec<PathBuf>> {
    let doc = open_mupdf_document(input, options.password.as_deref())?;
    let page_count = doc
        .page_count()
        .map_err(|source| mupdf_error_from_doc(&doc, source))?;
    let selected = options.pages.resolve(page_count as usize)?;

    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    let paths = output_paths(input, out_dir, &selected, page_count);
    let total = selected.len();

    std::fs::create_dir_all(out_dir).map_err(|source| PdfError::Io {
        path: out_dir.to_path_buf(),
        source,
    })?;

    for (index, (page, path)) in selected.iter().zip(&paths).enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(total),
                format!("writing {}", path.display()),
            ));
        }

        let svg = render_page_to_svg(&doc, *page)?;
        std::fs::write(path, svg).map_err(|source| PdfError::Io {
            path: path.clone(),
            source,
        })?;
    }

    Ok(paths)
}

#[cfg(feature = "render")]
fn open_mupdf_document(path: &Path, password: Option<&str>) -> Result<MuPdfDocument> {
    if !path.exists() {
        return Err(PdfError::NotFound(path.to_path_buf()));
    }

    let mut doc = MuPdfDocument::open(path).map_err(|source| mupdf_error(path, source))?;

    if doc
        .needs_password()
        .map_err(|source| mupdf_error(path, source))?
    {
        let Some(password) = password else {
            return Err(PdfError::Encrypted(path.to_path_buf()));
        };

        let accepted = doc
            .authenticate(password)
            .map_err(|source| mupdf_error(path, source))?;

        if !accepted {
            return Err(PdfError::WrongPassword(path.to_path_buf()));
        }
    }

    Ok(doc)
}

#[cfg(feature = "render")]
fn render_page_to_svg(doc: &MuPdfDocument, page_index: usize) -> Result<String> {
    let page_count = doc
        .page_count()
        .map_err(|source| mupdf_error_from_doc(doc, source))?;
    if page_index >= page_count as usize {
        return Err(PdfError::PageOutOfRange {
            requested: page_index + 1,
            total: page_count as usize,
        });
    }

    // Load the page
    let loaded = doc
        .load_page(page_index as i32)
        .map_err(|source| mupdf_error_from_doc(doc, source))?;

    // Use identity matrix for 1:1 mapping (72 DPI = 1 point per unit)
    // SVG is vector-based so the matrix mainly affects the viewBox
    let matrix = Matrix::IDENTITY;

    // Render page to SVG
    let svg = loaded
        .to_svg(&matrix)
        .map_err(|source| mupdf_error_from_doc(doc, source))?;

    // TODO: Post-process SVG for font embedding options and text_as_paths
    // For now, mupdf's to_svg() returns a basic SVG. Font embedding and
    // text-as-paths would require additional processing or MuPDF options.

    Ok(svg)
}

fn output_paths(
    input: &Path,
    out_dir: &Path,
    selected: &[usize],
    page_count: i32,
) -> Vec<PathBuf> {
    entry_names(input, selected, page_count as usize)
        .into_iter()
        .map(|name| out_dir.join(name))
        .collect()
}

/// The file names [`to_svg`] uses, without any directory:
/// `{stem}-{page}.svg`, zero-padded to the page count.
pub(crate) fn entry_names(input: &Path, selected: &[usize], page_count: usize) -> Vec<String> {
    let stem = input
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page".to_string());
    let width = page_count.max(1).to_string().len();

    selected
        .iter()
        .map(|page| format!("{stem}-{:0width$}.svg", page + 1, width = width))
        .collect()
}

#[cfg(feature = "render")]
fn mupdf_error(path: &Path, source: mupdf::Error) -> PdfError {
    PdfError::Render {
        path: path.to_path_buf(),
        reason: source.to_string(),
    }
}

#[cfg(feature = "render")]
fn mupdf_error_from_doc(_doc: &MuPdfDocument, source: mupdf::Error) -> PdfError {
    // We don't have the path here, but mupdf errors don't typically include the path anyway
    PdfError::Render {
        path: PathBuf::from("unknown"),
        reason: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn names_are_padded_to_the_page_count() {
        let options = SvgOptions::default();
        let paths = output_paths(
            Path::new("/in/report.pdf"),
            Path::new("/out"),
            &[0, 9],
            120,
        );

        assert_eq!(paths[0], Path::new("/out/report-001.svg"));
        assert_eq!(paths[1], Path::new("/out/report-010.svg"));
    }

    #[test]
    fn single_page_document_uses_width_of_1() {
        let options = SvgOptions::default();
        let paths = output_paths(
            Path::new("/in/single.pdf"),
            Path::new("/out"),
            &[0],
            1,
        );

        assert_eq!(paths[0], Path::new("/out/single-1.svg"));
    }
}