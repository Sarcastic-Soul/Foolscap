//! Recognition tests. Only built with the `ocr` feature.
//!
//! These skip themselves when Tesseract is not installed, so a machine without
//! it still gets a green run rather than a misleading failure.

#![cfg(feature = "ocr")]

mod common;

use common::{ImagePlacement, Workspace};
use pdf_core::ocr::{self, OcrOptions};
use pdf_core::render::PageRenderer;
use pdf_core::Document;

fn skip_without_tesseract(what: &str) -> bool {
    if ocr::is_available() {
        return false;
    }
    eprintln!("skipping {what}: Tesseract is not installed");
    true
}

/// A page that looks like a scan: an image of words, with no text layer.
///
/// The words are drawn into a PDF, rasterised, and the raster embedded as an
/// image. That is exactly what a scanner produces, and it is the only way to
/// get a fixture Tesseract has real work to do on.
fn scanned_page(workspace: &Workspace, name: &str, words: &str) -> std::path::PathBuf {
    use image::{ImageEncoder, Rgb, RgbImage};

    // Draw the words large enough for recognition, using the renderer so that
    // the glyphs are real ones rather than something hand-rolled.
    let source = workspace.write(
        &format!("{name}-source.pdf"),
        common::build_text_page(words, 48.0),
    );
    let renderer = PageRenderer::open(&source).unwrap();
    let rendered = renderer
        .render(0, pdf_core::render::Scale::Dpi(200.0))
        .unwrap();

    // Pages render as RGB on white, so the samples transfer straight across.
    let channels = rendered.channels as usize;
    let mut pixels = RgbImage::new(rendered.width, rendered.height);
    for (index, pixel) in pixels.pixels_mut().enumerate() {
        let offset = index * channels;
        *pixel = Rgb([
            rendered.pixels[offset],
            rendered.pixels[offset + 1],
            rendered.pixels[offset + 2],
        ]);
    }

    let mut encoded = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 92)
        .write_image(
            pixels.as_raw(),
            rendered.width,
            rendered.height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();

    workspace.write(
        name,
        common::build_with_jpeg(
            encoded.into_inner(),
            rendered.width,
            rendered.height,
            ImagePlacement::drawn(595.0, 842.0),
        ),
    )
}

#[test]
fn a_scanned_page_starts_with_no_text() {
    let workspace = Workspace::new();
    let scan = scanned_page(&workspace, "scan.pdf", "Foolscap");

    let renderer = PageRenderer::open(&scan).unwrap();
    assert!(
        !renderer.page_has_text(0).unwrap(),
        "the fixture is supposed to be an image of words, not text"
    );
}

#[test]
fn recognition_makes_a_scan_searchable() {
    if skip_without_tesseract("recognition_makes_a_scan_searchable") {
        return;
    }

    let workspace = Workspace::new();
    let scan = scanned_page(&workspace, "scan.pdf", "Foolscap");
    let out = workspace.join("searchable.pdf");

    let report = ocr::ocr(&scan, &out, &OcrOptions::default()).unwrap();

    assert_eq!(report.pages_recognised, 1, "report: {report:?}");

    let renderer = PageRenderer::open(&out).unwrap();
    let text = renderer.page_text(0).unwrap();
    assert!(
        text.to_lowercase().contains("foolscap"),
        "expected the recognised word, got {text:?}"
    );
}

#[test]
fn the_original_page_is_kept_intact() {
    if skip_without_tesseract("the_original_page_is_kept_intact") {
        return;
    }

    let workspace = Workspace::new();
    let scan = scanned_page(&workspace, "scan.pdf", "Foolscap");
    let out = workspace.join("searchable.pdf");

    let before = common::image_summaries(&scan);
    ocr::ocr(&scan, &out, &OcrOptions::default()).unwrap();
    let after = common::image_summaries(&out);

    // The whole point of a text-only layer: the scan is not re-rendered at
    // whatever resolution the recogniser happened to use.
    assert_eq!(before, after, "the scanned image should be untouched");
    assert_eq!(Document::open(&out).unwrap().page_count(), 1);
}

#[test]
fn a_page_that_already_has_text_is_left_alone() {
    if skip_without_tesseract("a_page_that_already_has_text_is_left_alone") {
        return;
    }

    let workspace = Workspace::new();
    let input = workspace.document("born-digital.pdf", 2, "Page");
    let out = workspace.join("out.pdf");

    let report = ocr::ocr(&input, &out, &OcrOptions::default()).unwrap();

    assert_eq!(report.pages_already_text, 2);
    assert_eq!(report.pages_recognised, 0);
}

#[test]
fn recognition_can_be_forced_onto_pages_that_have_text() {
    if skip_without_tesseract("recognition_can_be_forced_onto_pages_that_have_text") {
        return;
    }

    let workspace = Workspace::new();
    let input = workspace.document("born-digital.pdf", 1, "Page");
    let out = workspace.join("out.pdf");

    let report = ocr::ocr(
        &input,
        &out,
        &OcrOptions {
            skip_pages_with_text: false,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(report.pages_already_text, 0);
    assert_eq!(report.pages_recognised + report.pages_without_text, 1);
}

#[test]
fn an_uninstalled_language_is_refused_with_advice() {
    if skip_without_tesseract("an_uninstalled_language_is_refused_with_advice") {
        return;
    }

    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");

    let error = ocr::ocr(
        &input,
        &workspace.join("out.pdf"),
        &OcrOptions {
            language: "xyz".to_string(),
            ..Default::default()
        },
    )
    .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("xyz"), "got {message}");
    assert!(message.contains("apt install"), "got {message}");
}

#[test]
fn the_installed_languages_can_be_listed() {
    if skip_without_tesseract("the_installed_languages_can_be_listed") {
        return;
    }

    let languages = ocr::languages().unwrap();
    assert!(!languages.is_empty());
    // The header line must not have leaked into the list.
    assert!(
        !languages.iter().any(|code| code.contains(' ')),
        "got {languages:?}"
    );
}

#[test]
fn extracting_text_reads_what_a_page_already_carries() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 2, "Chapter");

    let renderer = PageRenderer::open(&input).unwrap();

    assert!(renderer.page_text(0).unwrap().contains("Chapter 1"));
    assert!(renderer.page_text(1).unwrap().contains("Chapter 2"));
}
