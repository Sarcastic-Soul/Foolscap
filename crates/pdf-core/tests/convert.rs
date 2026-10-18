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
    html_to_pdf, images_to_pdf, markdown_file_to_pdf, markdown_to_pdf, office, office_to_pdf,
    pdf_to_office, text_file_to_pdf, text_to_pdf, text_to_pdf_with_progress, Fit,
    ImagesToPdfOptions, OfficeFormat, PageSize, TextToPdfOptions,
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

    let produced = pdf_to_office(&input, OfficeFormat::Docx, workspace.path(), None).unwrap();

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

// ------------------------------------------------------ text to PDF

/// All the text a PDF shows, page by page, read back through MuPDF.
#[cfg(feature = "render")]
fn page_texts(path: &Path) -> Vec<String> {
    let renderer = pdf_core::PageRenderer::open(path).unwrap();
    (0..renderer.page_count())
        .map(|page| renderer.page_text(page).unwrap())
        .collect()
}

#[test]
fn text_becomes_a_one_page_pdf() {
    let workspace = Workspace::new();
    let out = workspace.join("out.pdf");

    text_to_pdf(
        "Foolscap text test.\nSecond line.",
        &out,
        &TextToPdfOptions::default(),
    )
    .unwrap();

    assert_eq!(Document::open(&out).unwrap().page_count(), 1);
    #[cfg(feature = "render")]
    {
        let texts = page_texts(&out);
        assert!(texts[0].contains("Foolscap text test."), "got {texts:?}");
        assert!(texts[0].contains("Second line."), "got {texts:?}");
    }
}

#[test]
fn long_text_paginates_in_order() {
    let workspace = Workspace::new();
    let out = workspace.join("out.pdf");
    let text: String = (1..=300).map(|n| format!("line number {n}\n")).collect();

    text_to_pdf(&text, &out, &TextToPdfOptions::default()).unwrap();

    let pages = Document::open(&out).unwrap().page_count();
    assert!((5..=8).contains(&pages), "got {pages} pages");
    #[cfg(feature = "render")]
    {
        let texts = page_texts(&out);
        assert!(texts[0].contains("line number 1"));
        assert!(texts.last().unwrap().contains("line number 300"));
        assert!(!texts[0].contains("line number 300"));
    }
}

#[test]
fn an_empty_text_is_still_a_valid_document() {
    let workspace = Workspace::new();
    let out = workspace.join("out.pdf");

    text_to_pdf("", &out, &TextToPdfOptions::default()).unwrap();

    assert_eq!(Document::open(&out).unwrap().page_count(), 1);
}

#[test]
fn the_text_page_size_and_title_are_honoured() {
    let workspace = Workspace::new();
    let out = workspace.join("out.pdf");

    text_to_pdf(
        "hello",
        &out,
        &TextToPdfOptions {
            page_size: PageSize::Letter,
            title: Some("My Title".to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    let boxes = common::page_media_boxes(&out);
    assert!((boxes[0][2] - 612).abs() <= 2, "got {:?}", boxes[0]);
    assert!((boxes[0][3] - 792).abs() <= 2, "got {:?}", boxes[0]);

    let title = Document::open(&out).unwrap().metadata().unwrap().title;
    assert_eq!(title.as_deref(), Some("My Title"));
}

#[test]
fn accented_and_unmappable_text_does_not_fail() {
    let workspace = Workspace::new();
    let out = workspace.join("out.pdf");

    text_to_pdf(
        "Café déjà vu, naïve résumé. Snowman: \u{2603} and \u{4e2d}\u{6587}.",
        &out,
        &TextToPdfOptions::default(),
    )
    .unwrap();

    #[cfg(feature = "render")]
    assert!(page_texts(&out)[0].contains("Café"), "accents were lost");
}

#[test]
fn text_progress_ends_with_the_final_write() {
    let workspace = Workspace::new();
    let text: String = (1..=200).map(|n| format!("line {n}\n")).collect();
    let mut seen = Vec::new();

    let mut report = |progress: pdf_core::Progress| seen.push(progress.message);
    text_to_pdf_with_progress(
        &text,
        &workspace.join("out.pdf"),
        &TextToPdfOptions::default(),
        Some(&mut report),
    )
    .unwrap();

    assert!(seen.len() >= 3, "got {seen:?}");
    assert_eq!(seen.last().map(String::as_str), Some("writing output"));
}

#[test]
fn a_bad_font_size_is_an_error() {
    let workspace = Workspace::new();

    for size in [0.0, -3.0, f32::NAN] {
        let result = text_to_pdf(
            "x",
            &workspace.join("out.pdf"),
            &TextToPdfOptions {
                font_size: size,
                ..Default::default()
            },
        );
        assert!(result.is_err(), "font size {size} was accepted");
    }
}

#[test]
fn a_text_file_becomes_a_pdf_titled_after_the_file() {
    let workspace = Workspace::new();
    let input = workspace.join("notes.txt");
    std::fs::write(&input, "Some notes.\n").unwrap();
    let out = workspace.join("out.pdf");

    text_file_to_pdf(&input, &out, &TextToPdfOptions::default()).unwrap();

    let title = Document::open(&out).unwrap().metadata().unwrap().title;
    assert_eq!(title.as_deref(), Some("notes"));
}

#[test]
fn a_missing_text_file_says_so() {
    let workspace = Workspace::new();
    let error = text_file_to_pdf(
        Path::new("/definitely/not/here.txt"),
        &workspace.join("out.pdf"),
        &TextToPdfOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
}

#[test]
fn a_text_file_that_is_not_utf8_is_an_io_error() {
    let workspace = Workspace::new();
    let input = workspace.join("binary.txt");
    std::fs::write(&input, [0xff, 0xfe, 0xfd, 0x00, 0x80]).unwrap();

    let error = text_file_to_pdf(
        &input,
        &workspace.join("out.pdf"),
        &TextToPdfOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, PdfError::Io { .. }), "got {error:?}");
}

// ------------------------------------------------- Markdown to PDF

#[test]
fn markdown_is_flattened_into_readable_text() {
    let workspace = Workspace::new();
    let out = workspace.join("out.pdf");

    markdown_to_pdf(
        "# Report Title\n\nSome **bold** text with a [link](https://example.com).\n\n- first\n- second\n",
        &out,
        &TextToPdfOptions::default(),
    )
    .unwrap();

    let doc = Document::open(&out).unwrap();
    assert_eq!(doc.page_count(), 1);
    // The first heading becomes the title.
    assert_eq!(
        doc.metadata().unwrap().title.as_deref(),
        Some("Report Title")
    );

    #[cfg(feature = "render")]
    {
        let text = &page_texts(&out)[0];
        assert!(text.contains("Report Title"), "got {text:?}");
        assert!(text.contains("Some bold text with a link."), "got {text:?}");
        assert!(text.contains("- first"), "got {text:?}");
        assert!(!text.contains("**") && !text.contains("example.com"));
    }
}

#[test]
fn a_markdown_file_without_a_heading_is_titled_after_the_file() {
    let workspace = Workspace::new();
    let input = workspace.join("readme.md");
    std::fs::write(&input, "just a paragraph\n").unwrap();
    let out = workspace.join("out.pdf");

    markdown_file_to_pdf(&input, &out, &TextToPdfOptions::default()).unwrap();

    let title = Document::open(&out).unwrap().metadata().unwrap().title;
    assert_eq!(title.as_deref(), Some("readme"));
}

#[test]
fn a_missing_markdown_file_says_so() {
    let workspace = Workspace::new();
    let error = markdown_file_to_pdf(
        Path::new("/definitely/not/here.md"),
        &workspace.join("out.pdf"),
        &TextToPdfOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
}

// ------------------------------------------------------------ HTML

#[test]
fn a_missing_html_file_says_so() {
    let error = html_to_pdf(
        Path::new("/definitely/not/here.html"),
        &std::env::temp_dir(),
    )
    .unwrap_err();

    assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
}

#[test]
fn an_html_file_becomes_a_pdf() {
    if skip_without_libreoffice("an_html_file_becomes_a_pdf") {
        return;
    }

    let workspace = Workspace::new();
    let input = workspace.join("page.html");
    std::fs::write(
        &input,
        "<!DOCTYPE html><html><head><title>T</title></head>\
         <body><h1>Heading</h1><p>Foolscap html test.</p></body></html>",
    )
    .unwrap();

    let produced = html_to_pdf(&input, workspace.path()).unwrap();

    assert_eq!(produced, workspace.join("page.pdf"));
    assert!(Document::open(&produced).unwrap().page_count() >= 1);
    #[cfg(feature = "render")]
    assert!(page_texts(&produced)[0].contains("Foolscap html test."));
}

#[cfg(feature = "render")]
mod with_rendering {
    use super::*;
    use pdf_core::convert::{
        pdf_to_html, pdf_to_html_with_progress, pdf_to_images, pdf_to_zip, plan, zip_plan,
        ImageFormat, PdfToHtmlOptions, PdfToImagesOptions,
    };
    use pdf_core::PageRange;

    // ------------------------------------------------- PDF to HTML

    #[test]
    fn a_pdf_becomes_one_html_section_per_page() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 3, "Alpha");
        let out = workspace.join("out/result.html");

        pdf_to_html(&input, &out, &PdfToHtmlOptions::default()).unwrap();

        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert_eq!(html.matches("<section class=\"page\"").count(), 3);
        for page in 1..=3 {
            assert!(html.contains(&format!("id=\"page-{page}\"")), "page {page}");
        }
        // The title falls back to the file stem.
        assert!(html.contains("<title>in</title>"), "got {html}");
    }

    #[test]
    fn an_explicit_title_is_escaped_into_the_html() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 1, "Alpha");
        let out = workspace.join("out.html");

        pdf_to_html(
            &input,
            &out,
            &PdfToHtmlOptions {
                title: Some("A <b> & C".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let html = std::fs::read_to_string(&out).unwrap();
        assert!(
            html.contains("<title>A &lt;b&gt; &amp; C</title>"),
            "got {html}"
        );
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn page_text_is_escaped_in_the_html() {
        let workspace = Workspace::new();
        let text = workspace.join("src.txt");
        std::fs::write(&text, "if a < b && c > d then stop\n").unwrap();
        let input = workspace.join("in.pdf");
        text_file_to_pdf(&text, &input, &TextToPdfOptions::default()).unwrap();
        let out = workspace.join("out.html");

        pdf_to_html(&input, &out, &PdfToHtmlOptions::default()).unwrap();

        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains("a &lt; b &amp;&amp; c &gt; d"), "got {html}");
    }

    #[test]
    fn pdf_to_html_reports_every_page_and_the_final_write() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 2, "Alpha");
        let mut messages = Vec::new();

        let mut report = |progress: pdf_core::Progress| messages.push(progress.message);
        pdf_to_html_with_progress(
            &input,
            &workspace.join("out.html"),
            &PdfToHtmlOptions::default(),
            Some(&mut report),
        )
        .unwrap();

        assert_eq!(messages.len(), 3, "got {messages:?}");
        assert_eq!(messages[2], "writing output");
    }

    #[test]
    fn pdf_to_html_on_a_missing_file_says_so() {
        let workspace = Workspace::new();
        let error = pdf_to_html(
            Path::new("/definitely/not/here.pdf"),
            &workspace.join("out.html"),
            &PdfToHtmlOptions::default(),
        )
        .unwrap_err();

        assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
    }

    // ---------------------------------------------- PDF to images

    #[test]
    fn pages_become_numbered_png_files() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 3, "Alpha");
        let out_dir = workspace.join("pages");

        let paths = pdf_to_images(
            &input,
            &out_dir,
            &PdfToImagesOptions {
                dpi: 50.0,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0], out_dir.join("in-1.png"));
        for path in &paths {
            let decoded = image::open(path).unwrap();
            // A4 at 50 dpi is about 413 by 585 pixels.
            assert!(
                (decoded.width() as i32 - 413).abs() <= 2,
                "{}",
                decoded.width()
            );
        }
    }

    #[test]
    fn only_the_chosen_pages_are_written_as_jpeg() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 12, "Alpha");
        let out_dir = workspace.join("pages");

        let paths = pdf_to_images(
            &input,
            &out_dir,
            &PdfToImagesOptions {
                pages: PageRange::parse("2,11-").unwrap(),
                dpi: 30.0,
                format: ImageFormat::Jpeg,
                ..Default::default()
            },
        )
        .unwrap();

        let names: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["in-02.jpg", "in-11.jpg", "in-12.jpg"]);
        assert!(paths.iter().all(|p| p.exists()));
    }

    #[test]
    fn planning_matches_what_is_written() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 2, "Alpha");
        let out_dir = workspace.join("pages");
        let options = PdfToImagesOptions {
            dpi: 30.0,
            ..Default::default()
        };

        let planned = plan(&input, &out_dir, &options).unwrap();
        assert!(!out_dir.exists(), "planning must not write anything");

        let written = pdf_to_images(&input, &out_dir, &options).unwrap();
        assert_eq!(planned, written);
    }

    #[test]
    fn an_unusable_resolution_is_refused() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 1, "Alpha");

        for dpi in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let error = pdf_to_images(
                &input,
                &workspace.join("pages"),
                &PdfToImagesOptions {
                    dpi,
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(
                matches!(error, PdfError::Render { .. }),
                "dpi {dpi}: {error:?}"
            );
        }
    }

    #[test]
    fn a_page_range_past_the_end_is_an_error() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 2, "Alpha");

        let result = pdf_to_images(
            &input,
            &workspace.join("pages"),
            &PdfToImagesOptions {
                pages: PageRange::parse("5").unwrap(),
                dpi: 30.0,
                ..Default::default()
            },
        );

        assert!(result.is_err());
    }

    // --------------------------------------------------- PDF to zip

    #[test]
    fn pages_bundle_into_a_zip_archive() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 3, "Alpha");
        let out = workspace.join("out/pages.zip");
        let options = PdfToImagesOptions {
            dpi: 30.0,
            ..Default::default()
        };

        let names = pdf_to_zip(&input, &out, &options).unwrap();

        assert_eq!(names, ["in-1.png", "in-2.png", "in-3.png"]);
        assert_eq!(zip_plan(&input, &options).unwrap(), names);

        let mut archive = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        assert_eq!(archive.len(), 3);
        for (index, name) in names.iter().enumerate() {
            let mut entry = archive.by_index(index).unwrap();
            assert_eq!(entry.name(), name);

            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
            assert!(
                image::load_from_memory(&bytes).is_ok(),
                "{name} is not an image"
            );
        }

        // Nothing but the archive was written.
        let leftovers: Vec<_> = std::fs::read_dir(workspace.join("out")).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[test]
    fn a_zip_of_a_page_subset_holds_only_those_pages() {
        let workspace = Workspace::new();
        let input = workspace.document("in.pdf", 4, "Alpha");
        let out = workspace.join("pages.zip");

        let names = pdf_to_zip(
            &input,
            &out,
            &PdfToImagesOptions {
                pages: PageRange::parse("2-3").unwrap(),
                dpi: 30.0,
                format: ImageFormat::Jpeg,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(names, ["in-2.jpg", "in-3.jpg"]);
        let archive = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        assert_eq!(archive.len(), 2);
    }

    #[test]
    fn zipping_a_missing_file_says_so() {
        let workspace = Workspace::new();
        let error = pdf_to_zip(
            Path::new("/definitely/not/here.pdf"),
            &workspace.join("out.zip"),
            &PdfToImagesOptions::default(),
        )
        .unwrap_err();

        assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
        assert!(!workspace.join("out.zip").exists());
    }

    // ------------------------------------------------- passwords

    fn protected(workspace: &Workspace) -> PathBuf {
        let mut doc = common::build(2, "Secret", common::A4, common::Attributes::OnPage);
        common::encrypt(&mut doc, common::Cipher::Aes256, "open sesame", "owner");
        workspace.write("locked.pdf", doc)
    }

    #[test]
    fn a_protected_pdf_needs_its_password_for_every_output() {
        let workspace = Workspace::new();
        let input = protected(&workspace);
        let options = PdfToImagesOptions {
            dpi: 30.0,
            ..Default::default()
        };

        let error = pdf_to_html(
            &input,
            &workspace.join("a.html"),
            &PdfToHtmlOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(error, PdfError::Encrypted(_)), "got {error:?}");

        let error = pdf_to_images(&input, &workspace.join("pages"), &options).unwrap_err();
        assert!(matches!(error, PdfError::Encrypted(_)), "got {error:?}");

        let error = pdf_to_zip(&input, &workspace.join("a.zip"), &options).unwrap_err();
        assert!(matches!(error, PdfError::Encrypted(_)), "got {error:?}");

        let wrong = PdfToImagesOptions {
            password: Some("nope".to_string()),
            ..options.clone()
        };
        let error = pdf_to_zip(&input, &workspace.join("a.zip"), &wrong).unwrap_err();
        assert!(matches!(error, PdfError::WrongPassword(_)), "got {error:?}");
        assert!(!workspace.join("a.zip").exists());
    }

    #[test]
    fn the_right_password_unlocks_every_output() {
        let workspace = Workspace::new();
        let input = protected(&workspace);
        let password = Some("open sesame".to_string());

        pdf_to_html(
            &input,
            &workspace.join("a.html"),
            &PdfToHtmlOptions {
                password: password.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        let html = std::fs::read_to_string(workspace.join("a.html")).unwrap();
        assert!(
            html.contains("Secret 1") && html.contains("Secret 2"),
            "got {html}"
        );

        let options = PdfToImagesOptions {
            dpi: 30.0,
            password,
            ..Default::default()
        };
        assert_eq!(
            pdf_to_images(&input, &workspace.join("pages"), &options)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            pdf_to_zip(&input, &workspace.join("a.zip"), &options)
                .unwrap()
                .len(),
            2
        );
    }
}
