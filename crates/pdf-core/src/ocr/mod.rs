//! Optical character recognition, via Tesseract.
//!
//! Needs the `ocr` feature and, because pages have to be rasterised before they
//! can be recognised, the `render` feature too.
//!
//! The result is the original document with an invisible text layer added, not
//! a rebuilt one: the pages keep whatever quality they arrived with, and only
//! become searchable.

mod overlay;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::progress::{Progress, ProgressFn};
use crate::render::{PageRenderer, Scale};
use crate::subprocess::TESSERACT;

/// How long one page may take to recognise.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Recognition resolution. Tesseract is trained around 300 dpi and does
/// noticeably worse below it; higher costs time without helping much.
pub const DEFAULT_DPI: f32 = 300.0;

/// Options for [`ocr`].
#[derive(Debug, Clone, PartialEq)]
pub struct OcrOptions {
    /// Tesseract language code, or several joined with `+`.
    pub language: String,
    /// Resolution to rasterise pages at before recognising them.
    pub dpi: f32,
    /// Leave pages that already carry text alone. Almost always what you want:
    /// a born-digital page has perfect text already, and recognising it again
    /// can only make it worse.
    pub skip_pages_with_text: bool,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            language: "eng".to_string(),
            dpi: DEFAULT_DPI,
            skip_pages_with_text: true,
        }
    }
}

/// What a recognition pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OcrReport {
    pub pages_total: usize,
    pub pages_recognised: usize,
    /// Pages left alone because they already had text.
    pub pages_already_text: usize,
    /// Pages where the recogniser found nothing, such as a blank scan.
    pub pages_without_text: usize,
}

/// Whether Tesseract is installed and usable.
pub fn is_available() -> bool {
    TESSERACT.is_available()
}

/// Language codes Tesseract has data for.
pub fn languages() -> Result<Vec<String>> {
    let output = TESSERACT.run(&["--list-langs".to_string()], Duration::from_secs(30))?;

    // The first line is a header; the rest are codes.
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Add an invisible text layer to `input`, writing the result to `output`.
pub fn ocr(input: &Path, output: &Path, options: &OcrOptions) -> Result<OcrReport> {
    ocr_with_progress(input, output, options, None)
}

/// [`ocr`], reporting one progress tick per page.
pub fn ocr_with_progress(
    input: &Path,
    output: &Path,
    options: &OcrOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<OcrReport> {
    if !(options.dpi.is_finite() && options.dpi > 0.0) {
        return Err(PdfError::Render {
            path: input.to_path_buf(),
            reason: format!("{} is not a usable resolution", options.dpi),
        });
    }

    check_language(&options.language)?;

    let renderer = PageRenderer::open(input)?;
    let mut doc = Document::open(input)?;

    let page_ids: Vec<lopdf::ObjectId> = doc.inner.get_pages().into_values().collect();
    let total = page_ids.len();

    let mut report = OcrReport {
        pages_total: total,
        ..Default::default()
    };

    let scratch = Scratch::new()?;

    for (index, page_id) in page_ids.into_iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(total),
                format!("page {} of {total}", index + 1),
            ));
        }

        if options.skip_pages_with_text && renderer.page_has_text(index)? {
            report.pages_already_text += 1;
            continue;
        }

        let recognised = recognise_page(&renderer, index, options, &scratch)?;

        let Some(layer) = recognised else {
            report.pages_without_text += 1;
            continue;
        };

        // The text layer's coordinates are in the rasterised page's space. Both
        // derive from the same page at a known resolution, so the ratio of page
        // widths is the whole correction.
        let (page_width, _) = renderer.page_size(index)?;
        let layer_width = layer_page_width(&layer.document).unwrap_or(page_width);
        let scale = if layer_width > 0.0 {
            page_width / layer_width
        } else {
            1.0
        };

        overlay::graft_text_layer(&mut doc.inner, &layer.document, page_id, scale)?;
        report.pages_recognised += 1;
    }

    doc.save(output)?;

    tracing::debug!(
        recognised = report.pages_recognised,
        already_text = report.pages_already_text,
        blank = report.pages_without_text,
        "recognition complete"
    );

    Ok(report)
}

/// Fail early and clearly when the requested language is not installed, rather
/// than letting Tesseract fail per page with its own wording.
fn check_language(language: &str) -> Result<()> {
    let installed = languages()?;

    for part in language.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        if !installed.iter().any(|code| code == part) {
            return Err(PdfError::ToolFailed {
                tool: TESSERACT.name,
                status: None,
                message: format!(
                    "language {part:?} is not installed; available: {}. \
                     Install more with: sudo apt install tesseract-ocr-{part}",
                    installed.join(", ")
                ),
            });
        }
    }

    Ok(())
}

struct TextLayer {
    document: lopdf::Document,
}

/// Rasterise one page and recognise it, returning the text-only PDF Tesseract
/// produced, or `None` when nothing was found.
fn recognise_page(
    renderer: &PageRenderer,
    page: usize,
    options: &OcrOptions,
    scratch: &Scratch,
) -> Result<Option<TextLayer>> {
    let rendered = renderer.render(page, Scale::Dpi(options.dpi))?;

    let image_path = scratch.path.join(format!("page-{page}.png"));
    write_png(&rendered, &image_path)?;

    // Tesseract appends the extension itself, so it wants a base name.
    let output_base = scratch.path.join(format!("page-{page}-text"));

    let arguments = vec![
        image_path.to_string_lossy().into_owned(),
        output_base.to_string_lossy().into_owned(),
        "-l".to_string(),
        options.language.clone(),
        "--dpi".to_string(),
        options.dpi.round().to_string(),
        // Produce the text layer alone, with no copy of the image. Without
        // this, Tesseract writes a whole page including its own rendering,
        // and grafting that on would replace the original page's quality with
        // the recogniser's.
        "-c".to_string(),
        "textonly_pdf=1".to_string(),
        "pdf".to_string(),
    ];

    TESSERACT.run(&arguments, TIMEOUT)?;

    let produced = output_base.with_extension("pdf");
    if !produced.exists() {
        return Err(PdfError::ToolFailed {
            tool: TESSERACT.name,
            status: None,
            message: format!("produced no output for page {}", page + 1),
        });
    }

    let document = lopdf::Document::load(&produced).map_err(|error| {
        PdfError::Internal(format!("could not read the recognised text layer: {error}"))
    })?;

    // Clean up as we go: a long document at 300 dpi would otherwise leave
    // hundreds of megabytes of intermediates in the temporary directory.
    let _ = std::fs::remove_file(&image_path);
    let _ = std::fs::remove_file(&produced);

    Ok(Some(TextLayer { document }))
}

/// The width of the text layer's page, in points.
fn layer_page_width(doc: &lopdf::Document) -> Option<f32> {
    let page_id = doc.get_pages().into_values().next()?;
    let dict = doc.get_dictionary(page_id).ok()?;
    let media_box = dict.get(b"MediaBox").ok()?.as_array().ok()?;

    if media_box.len() < 4 {
        return None;
    }

    let value = |index: usize| -> Option<f32> {
        match &media_box[index] {
            lopdf::Object::Integer(number) => Some(*number as f32),
            lopdf::Object::Real(number) => Some(*number),
            _ => None,
        }
    };

    Some(value(2)? - value(0)?)
}

fn write_png(page: &crate::render::RenderedPage, path: &Path) -> Result<()> {
    use image::{ExtendedColorType, ImageEncoder};

    let colour = match page.channels {
        4 => ExtendedColorType::Rgba8,
        3 => ExtendedColorType::Rgb8,
        1 => ExtendedColorType::L8,
        other => {
            return Err(PdfError::Render {
                path: path.to_path_buf(),
                reason: format!("{other} channels per pixel"),
            })
        }
    };

    let file = std::fs::File::create(path).map_err(|source| PdfError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file))
        .write_image(&page.pixels, page.width, page.height, colour)
        .map_err(|source| PdfError::Render {
            path: path.to_path_buf(),
            reason: source.to_string(),
        })
}

/// A scratch directory for the rasterised pages and Tesseract's output.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let path = std::env::temp_dir().join(format!(
            "foolscap-ocr-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));

        std::fs::create_dir_all(&path).map_err(|source| PdfError::Io {
            path: path.clone(),
            source,
        })?;

        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn the_default_targets_the_resolution_tesseract_expects() {
        let options = OcrOptions::default();
        assert_eq!(options.dpi, 300.0);
        assert_eq!(options.language, "eng");
        assert!(options.skip_pages_with_text);
    }

    #[test]
    fn a_missing_language_is_reported_before_any_work() {
        if !is_available() {
            return;
        }

        let error = check_language("definitely-not-a-language").unwrap_err();
        match error {
            PdfError::ToolFailed { message, .. } => {
                assert!(message.contains("not installed"), "got {message}");
                assert!(message.contains("apt install"), "should say how to fix it");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn an_installed_language_is_accepted() {
        if !is_available() {
            return;
        }

        let installed = languages().unwrap();
        if let Some(first) = installed.first() {
            check_language(first).unwrap();
        }
    }

    #[test]
    fn the_page_width_comes_from_the_media_box() {
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Count" => 1,
                "Kids" => vec![page.into()],
            }),
        );

        assert_eq!(layer_page_width(&doc), Some(612.0));
    }

    #[test]
    fn a_scratch_directory_cleans_itself_up() {
        let path = {
            let scratch = Scratch::new().unwrap();
            scratch.path.clone()
        };

        assert!(!path.exists());
    }
}
