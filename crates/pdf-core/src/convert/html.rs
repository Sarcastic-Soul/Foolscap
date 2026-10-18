//! HTML in both directions.
//!
//! Into PDF goes through headless LibreOffice, exactly like any other office
//! format: it needs LibreOffice installed at run time, and reports
//! [`PdfError::ToolMissing`](crate::PdfError::ToolMissing) when it is absent
//! so that callers can grey the option out rather than failing mid-task.
//!
//! Out of PDF is dependency-free: the text the document already carries is
//! read back and wrapped in one section per page.

use std::path::{Path, PathBuf};

#[cfg(feature = "render")]
use crate::error::PdfError;
use crate::error::Result;
#[cfg(feature = "render")]
use crate::progress::{Progress, ProgressFn};

/// Convert an HTML file into a PDF in `out_dir`, returning the path written.
///
/// A thin wrapper over [`crate::convert::office_to_pdf`]: LibreOffice does
/// the work, with the same isolated profile and timeout.
pub fn html_to_pdf(input: &Path, out_dir: &Path) -> Result<PathBuf> {
    crate::convert::office_to_pdf(input, out_dir)
}

/// Whether HTML to PDF conversion is possible right now, which is whether
/// LibreOffice is installed.
pub fn is_available() -> bool {
    crate::convert::office::is_available()
}

/// Options for [`pdf_to_html`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PdfToHtmlOptions {
    /// Page title. Falls back to the document metadata, then the file stem.
    pub title: Option<String>,
    /// Password for an encrypted input, used only if the file needs one.
    pub password: Option<String>,
}

/// Convert a PDF's embedded text into a one-section-per-page HTML file.
///
/// Needs the `render` feature: page text comes from MuPDF. A scanned page
/// with no text layer contributes an empty section rather than an error, so
/// the page numbering of the result still matches the document.
#[cfg(feature = "render")]
pub fn pdf_to_html(input: &Path, output: &Path, options: &PdfToHtmlOptions) -> Result<()> {
    pdf_to_html_with_progress(input, output, options, None)
}

/// [`pdf_to_html`], reporting one progress tick per page.
#[cfg(feature = "render")]
pub fn pdf_to_html_with_progress(
    input: &Path,
    output: &Path,
    options: &PdfToHtmlOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    use crate::document::Document;
    use crate::render::PageRenderer;

    if !input.exists() {
        return Err(PdfError::NotFound(input.to_path_buf()));
    }

    let renderer = PageRenderer::open_with(input, options.password.as_deref())?;
    let total = renderer.page_count();

    let title = options.title.clone().or_else(|| {
        Document::open_with(input, options.password.as_deref())
            .ok()
            .and_then(|doc| doc.metadata().ok())
            .and_then(|meta| meta.title)
    });
    let title = title.unwrap_or_else(|| {
        input
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".to_string())
    });

    let mut body = String::new();
    for page in 0..total {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                page,
                Some(total),
                format!("reading page {} of {total}", page + 1),
            ));
        }

        let text = renderer.page_text(page)?;
        body.push_str(&format!(
            "<section class=\"page\" id=\"page-{}\">\n<h2>Page {}</h2>\n<pre>{}</pre>\n</section>\n",
            page + 1,
            page + 1,
            escape_html(text.trim()),
        ));
    }

    if let Some(tick) = progress.as_mut() {
        tick(Progress::new(total, Some(total), "writing output"));
    }

    let document = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n</head>\n<body>\n<h1>{}</h1>\n{}\
         </body>\n</html>\n",
        escape_html(&title),
        escape_html(&title),
        body,
    );

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| PdfError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    std::fs::write(output, document).map_err(|source| PdfError::Io {
        path: output.to_path_buf(),
        source,
    })
}

/// Escape the five characters that are special in HTML text.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PdfError;

    #[test]
    fn the_five_special_characters_are_escaped() {
        assert_eq!(escape_html("&<>\"'"), "&amp;&lt;&gt;&quot;&#39;");
    }

    #[test]
    fn ordinary_text_passes_through() {
        assert_eq!(escape_html("Hello, world."), "Hello, world.");
    }

    #[test]
    fn converting_a_missing_file_says_so() {
        let error = html_to_pdf(
            Path::new("/definitely/not/here.html"),
            &std::env::temp_dir(),
        )
        .unwrap_err();

        assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
    }
}
