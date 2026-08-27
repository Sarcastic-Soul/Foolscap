//! Conversion tests. Only built with the `convert` feature.
//!
//! The office tests need LibreOffice installed and skip themselves when it is
//! absent, so that a machine without it still gets a green run rather than a
//! misleading failure.

#![cfg(feature = "convert")]

mod common;

use std::path::{Path, PathBuf};

use common::Workspace;
use pdf_core::convert::{
    images_to_pdf, office, office_to_pdf, pdf_to_office, Fit, ImagesToPdfOptions, OfficeFormat,
    PageSize,
};
use pdf_core::{Document, PdfError};

/// Write a PNG of the given size into the workspace.
fn png(workspace: &Workspace, name: &str, width: u32, height: u32) -> PathBuf {
    use image::{ImageEncoder, Rgb, RgbImage};

    let mut pixels = RgbImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
    }

    let path = workspace.join(name);
    let file = std::fs::File::create(&path).unwrap();
    image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file))
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();

    path
}

fn skip_without_libreoffice(what: &str) -> bool {
    if office::is_available() {
        return false;
    }
    eprintln!("skipping {what}: LibreOffice is not installed");
    true
}

// -------------------------------------------------------- images to PDF

#[test]
fn one_page_per_image() {
    let workspace = Workspace::new();
    let inputs = vec![
        png(&workspace, "a.png", 400, 300),
        png(&workspace, "b.png", 300, 400),
        png(&workspace, "c.png", 500, 500),
    ];
    let out = workspace.join("out.pdf");

    images_to_pdf(&inputs, &out, ImagesToPdfOptions::default()).unwrap();

    assert_eq!(Document::open(&out).unwrap().page_count(), 3);
}

#[test]
fn the_page_size_is_the_one_that_was_asked_for() {
    let workspace = Workspace::new();
    let inputs = vec![png(&workspace, "a.png", 400, 300)];
    let out = workspace.join("out.pdf");

    images_to_pdf(
        &inputs,
        &out,
        ImagesToPdfOptions {
            page_size: PageSize::Letter,
            ..Default::default()
        },
    )
    .unwrap();

    let boxes = common::page_media_boxes(&out);
    assert_eq!(boxes.len(), 1);
    // Letter is 612 by 792 points, allowing for the millimetre round trip.
    assert!((boxes[0][2] - 612).abs() <= 2, "got {:?}", boxes[0]);
    assert!((boxes[0][3] - 792).abs() <= 2, "got {:?}", boxes[0]);
}

#[test]
fn fit_image_gives_each_page_the_shape_of_its_image() {
    let workspace = Workspace::new();
    // 600 by 300 pixels at 300 dpi is 2 inches by 1, so 144 by 72 points.
    let inputs = vec![png(&workspace, "wide.png", 600, 300)];
    let out = workspace.join("out.pdf");

    images_to_pdf(
        &inputs,
        &out,
        ImagesToPdfOptions {
            page_size: PageSize::FitImage,
            dpi: 300.0,
            ..Default::default()
        },
    )
    .unwrap();

    let boxes = common::page_media_boxes(&out);
    assert!((boxes[0][2] - 144).abs() <= 2, "got {:?}", boxes[0]);
    assert!((boxes[0][3] - 72).abs() <= 2, "got {:?}", boxes[0]);
}

#[test]
fn mixed_orientations_all_land_on_the_page() {
    let workspace = Workspace::new();
    let inputs = vec![
        png(&workspace, "landscape.png", 800, 200),
        png(&workspace, "portrait.png", 200, 800),
    ];
    let out = workspace.join("out.pdf");

    images_to_pdf(
        &inputs,
        &out,
        ImagesToPdfOptions {
            page_size: PageSize::A4,
            fit: Fit::Contain,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(Document::open(&out).unwrap().page_count(), 2);
    for media_box in common::page_media_boxes(&out) {
        assert!((media_box[2] - 595).abs() <= 2, "got {media_box:?}");
    }
}

#[test]
fn converting_no_images_is_an_error() {
    let workspace = Workspace::new();
    let error = images_to_pdf(
        &[],
        &workspace.join("out.pdf"),
        ImagesToPdfOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, PdfError::EmptySelection), "got {error:?}");
}

#[test]
fn a_file_that_is_not_an_image_is_rejected() {
    let workspace = Workspace::new();
    let path = workspace.join("not-an-image.png");
    std::fs::write(&path, b"this is not a PNG").unwrap();

    let error = images_to_pdf(
        &[path],
        &workspace.join("out.pdf"),
        ImagesToPdfOptions::default(),
    )
    .unwrap_err();

    assert!(
        matches!(error, PdfError::UnsupportedImage(_) | PdfError::Io { .. }),
        "got {error:?}"
    );
}

#[test]
fn the_result_of_an_image_conversion_can_be_reopened() {
    let workspace = Workspace::new();
    let inputs = vec![png(&workspace, "a.png", 400, 300)];
    let out = workspace.join("out.pdf");

    images_to_pdf(&inputs, &out, ImagesToPdfOptions::default()).unwrap();

    // The whole point is that other Foolscap operations can take it from here.
    let split_dir = workspace.join("split");
    let pieces = pdf_core::split(&out, &pdf_core::SplitSpec::Every(1), &split_dir).unwrap();
    assert_eq!(pieces.len(), 1);
}

// ---------------------------------------------------------- office

#[test]
fn a_text_document_becomes_a_pdf() {
    if skip_without_libreoffice("a_text_document_becomes_a_pdf") {
        return;
    }

    let workspace = Workspace::new();
    let input = workspace.join("note.txt");
    std::fs::write(&input, "Foolscap conversion test.\nSecond line.\n").unwrap();

    let produced = office_to_pdf(&input, workspace.path()).unwrap();

    assert_eq!(produced, workspace.join("note.pdf"));
    assert!(Document::open(&produced).unwrap().page_count() >= 1);
}

#[test]
fn a_pdf_becomes_an_editable_document() {
    if skip_without_libreoffice("a_pdf_becomes_an_editable_document") {
        return;
    }

    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");

    let produced = pdf_to_office(&input, OfficeFormat::Docx, workspace.path()).unwrap();

    assert!(produced.exists());
    assert_eq!(produced.extension().unwrap(), "docx");
    // A docx is a zip; an empty conversion would be a few hundred bytes of
    // scaffolding, so require enough content to prove text came through.
    assert!(
        std::fs::metadata(&produced).unwrap().len() > 1000,
        "the conversion looks empty"
    );
}

#[test]
fn concurrent_conversions_do_not_collide() {
    // Two headless LibreOffice processes sharing a user profile do not queue:
    // the second attaches to the first and then hangs or exits having done
    // nothing. Each conversion gets its own profile, so both must succeed.
    if skip_without_libreoffice("concurrent_conversions_do_not_collide") {
        return;
    }

    let workspace = Workspace::new();
    let mut inputs = Vec::new();
    for index in 0..2 {
        let path = workspace.join(&format!("note-{index}.txt"));
        std::fs::write(&path, format!("Document number {index}.\n")).unwrap();
        inputs.push(path);
    }

    let directory = workspace.path().to_path_buf();
    let handles: Vec<_> = inputs
        .into_iter()
        .map(|input| {
            let directory = directory.clone();
            std::thread::spawn(move || office_to_pdf(&input, &directory))
        })
        .collect();

    for handle in handles {
        let produced = handle
            .join()
            .expect("thread panicked")
            .expect("conversion failed");
        assert!(produced.exists(), "{} was not written", produced.display());
    }
}

#[test]
fn converting_a_missing_document_says_so() {
    let error = office_to_pdf(
        Path::new("/definitely/not/here.docx"),
        &std::env::temp_dir(),
    )
    .unwrap_err();

    assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
}
