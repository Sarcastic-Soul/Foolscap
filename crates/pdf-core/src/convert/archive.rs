//! PDF pages as a zip archive of images. Needs both `convert` and `render`.
//!
//! The same pages [`super::raster::pdf_to_images`] would write, with the same
//! names, bundled into one archive instead of a directory. Each page is
//! encoded in memory and written straight into the archive, so no image file
//! ever touches the disk.

use std::io::Write;
use std::path::Path;

use crate::convert::raster::{entry_names, PdfToImagesOptions};
use crate::error::{PdfError, Result};
use crate::progress::{Progress, ProgressFn};
use crate::render::{PageRenderer, Scale};

/// The archive entry names [`pdf_to_zip`] would write, without writing
/// anything.
///
/// The CLI needs these in advance to describe what the archive will hold.
pub fn zip_plan(input: &Path, options: &PdfToImagesOptions) -> Result<Vec<String>> {
    let renderer = PageRenderer::open_with(input, options.password.as_deref())?;
    let selected = options.pages.resolve(renderer.page_count())?;

    Ok(entry_names(
        input,
        &selected,
        renderer.page_count(),
        options,
    ))
}

/// Bundle the selected pages into a zip archive at `output`.
///
/// Returns the entry names in archive order.
pub fn pdf_to_zip(
    input: &Path,
    output: &Path,
    options: &PdfToImagesOptions,
) -> Result<Vec<String>> {
    pdf_to_zip_with_progress(input, output, options, None)
}

/// [`pdf_to_zip`], reporting one progress tick per page.
pub fn pdf_to_zip_with_progress(
    input: &Path,
    output: &Path,
    options: &PdfToImagesOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<Vec<String>> {
    use crate::convert::raster::encode_image;

    if !(options.dpi.is_finite() && options.dpi > 0.0) {
        return Err(PdfError::Render {
            path: input.to_path_buf(),
            reason: format!("{} is not a usable resolution", options.dpi),
        });
    }

    let renderer = PageRenderer::open_with(input, options.password.as_deref())?;
    let selected = options.pages.resolve(renderer.page_count())?;

    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    let names = entry_names(input, &selected, renderer.page_count(), options);
    let total = selected.len();

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| PdfError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    let file = std::fs::File::create(output).map_err(|source| PdfError::Io {
        path: output.to_path_buf(),
        source,
    })?;
    let mut archive = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let entry_options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    for (index, (page, name)) in selected.iter().zip(&names).enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(index, Some(total), format!("adding {name}")));
        }

        let rendered = renderer.render(*page, Scale::Dpi(options.dpi))?;
        // `encode_image` wants a path only to name errors after; the entry
        // name identifies the failure just as well.
        let bytes = encode_image(&rendered, Path::new(name), options)?;

        archive
            .start_file(name, entry_options)
            .map_err(|source| PdfError::Internal(format!("could not add {name}: {source}")))?;
        archive.write_all(&bytes).map_err(|source| PdfError::Io {
            path: output.to_path_buf(),
            source,
        })?;
    }

    archive.finish().map_err(|source| PdfError::Io {
        path: output.to_path_buf(),
        source: std::io::Error::other(source.to_string()),
    })?;

    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::PageRange;

    fn options() -> PdfToImagesOptions {
        PdfToImagesOptions::default()
    }

    #[test]
    fn entry_names_follow_the_raster_scheme() {
        let names = entry_names(Path::new("/in/report.pdf"), &[0, 9], 120, &options());

        assert_eq!(names, vec!["report-001.png", "report-010.png"]);
    }

    #[test]
    fn an_unusable_resolution_is_rejected_before_any_work() {
        let error = pdf_to_zip(
            Path::new("/definitely/not/here.pdf"),
            Path::new("/tmp/out.zip"),
            &PdfToImagesOptions {
                dpi: f32::NAN,
                ..options()
            },
        )
        .unwrap_err();

        assert!(matches!(error, PdfError::Render { .. }), "got {error:?}");
    }

    #[test]
    fn planning_a_missing_file_says_so() {
        let error = zip_plan(
            Path::new("/definitely/not/here.pdf"),
            &PdfToImagesOptions {
                pages: PageRange::All,
                ..options()
            },
        )
        .unwrap_err();

        assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
    }
}
