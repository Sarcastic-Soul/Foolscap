//! Tests for the lossy image compression pass.

mod common;

use common::{ImagePlacement, Predictor, Workspace};
use pdf_core::{CompressLevel, Document, SkipReason};

/// The workhorse case: a large photo drawn into a small box on the page.
///
/// 1000 pixels across a 100 point box is 720 dpi, so every level should have
/// something to take away.
fn oversampled(workspace: &Workspace, name: &str) -> std::path::PathBuf {
    workspace.write(
        name,
        common::build_with_image(1000, 1000, ImagePlacement::drawn(100.0, 100.0)),
    )
}

#[test]
fn an_oversampled_image_is_resampled_to_the_target_resolution() {
    let workspace = Workspace::new();
    let input = oversampled(&workspace, "in.pdf");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();
    doc.save(&out).unwrap();

    assert_eq!(report.images_recompressed, 1);

    let images = common::image_summaries(&out);
    assert_eq!(images.len(), 1);

    // 720 dpi down to 72 is a tenth, so roughly 100 pixels across.
    let (width, height, _) = images[0];
    assert!(
        (95..=105).contains(&width),
        "expected about 100 pixels, got {width}"
    );
    assert_eq!(width, height, "aspect ratio should be preserved");
}

#[test]
fn each_level_targets_its_own_resolution() {
    let workspace = Workspace::new();

    let widths: Vec<u32> = [
        CompressLevel::Screen,
        CompressLevel::Ebook,
        CompressLevel::Print,
    ]
    .into_iter()
    .map(|level| {
        let input = oversampled(&workspace, &format!("in-{level:?}.pdf"));
        let out = workspace.join(&format!("out-{level:?}.pdf"));

        let mut doc = Document::open(&input).unwrap();
        pdf_core::compress(&mut doc, level).unwrap();
        doc.save(&out).unwrap();

        common::image_summaries(&out)[0].0
    })
    .collect();

    assert!(
        widths[0] < widths[1] && widths[1] < widths[2],
        "screen < ebook < print, got {widths:?}"
    );
}

#[test]
fn the_file_actually_gets_smaller() {
    let workspace = Workspace::new();
    let input = oversampled(&workspace, "in.pdf");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();
    doc.save(&out).unwrap();

    let before = std::fs::metadata(&input).unwrap().len();
    let after = std::fs::metadata(&out).unwrap().len();

    assert!(after < before, "{after} should be less than {before}");
    assert!(
        report.ratio_saved() > 0.5,
        "a tenth-scale resample should save most of the file, saved {:.1}%",
        report.ratio_saved() * 100.0
    );
    assert_eq!(report.bytes_after, after);
}

#[test]
fn an_image_already_at_the_target_is_left_alone() {
    let workspace = Workspace::new();
    // 150 pixels across a 72 point box is 150 dpi, exactly the ebook target.
    let input = workspace.write(
        "in.pdf",
        common::build_with_image(150, 150, ImagePlacement::drawn(72.0, 72.0)),
    );

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Ebook).unwrap();

    assert_eq!(report.images_recompressed, 0);
    assert_eq!(report.skipped.get(&SkipReason::AlreadySmall), Some(&1));
}

#[test]
fn an_image_that_is_never_drawn_is_left_alone() {
    // Nothing invokes it, so there is no placement to judge its resolution by.
    let workspace = Workspace::new();
    let input = workspace.write(
        "in.pdf",
        common::build_with_image(1000, 1000, ImagePlacement::undrawn()),
    );

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();

    assert_eq!(report.images_recompressed, 0);
    assert_eq!(report.skipped.get(&SkipReason::NeverDrawn), Some(&1));
}

#[test]
fn a_tiny_image_is_left_alone() {
    let workspace = Workspace::new();
    let input = workspace.write(
        "in.pdf",
        common::build_with_image(32, 32, ImagePlacement::drawn(4.0, 4.0)),
    );

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();

    assert_eq!(report.images_recompressed, 0);
    assert_eq!(report.skipped.get(&SkipReason::Tiny), Some(&1));
}

#[test]
fn a_stencil_mask_is_left_alone() {
    let workspace = Workspace::new();

    let mut doc = common::build_with_image(1000, 1000, ImagePlacement::drawn(100.0, 100.0));
    // Mark the image as a stencil after the fact.
    let image_id = *doc
        .objects
        .iter()
        .find(|(_, object)| match object {
            lopdf::Object::Stream(stream) => {
                stream.dict.get(b"Subtype").and_then(|s| s.as_name()).ok()
                    == Some(b"Image".as_slice())
            }
            _ => false,
        })
        .map(|(id, _)| id)
        .unwrap();
    if let Ok(lopdf::Object::Stream(stream)) = doc.get_object_mut(image_id) {
        stream.dict.set("ImageMask", lopdf::Object::Boolean(true));
    }

    let input = workspace.write("in.pdf", doc);

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();

    assert_eq!(report.images_recompressed, 0);
    assert_eq!(report.skipped.get(&SkipReason::Mask), Some(&1));
}

#[test]
fn a_document_without_images_still_runs_the_lossless_pass() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();
    doc.save(&out).unwrap();

    assert_eq!(report.images_examined, 0);
    assert_eq!(common::page_labels(&out), ["Page 1", "Page 2", "Page 3"]);
}

#[test]
fn compression_keeps_the_page_structure_intact() {
    let workspace = Workspace::new();
    let input = oversampled(&workspace, "in.pdf");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();
    doc.save(&out).unwrap();

    let reopened = Document::open(&out).unwrap();
    assert_eq!(reopened.page_count(), 1);
    assert_eq!(
        common::page_media_boxes(&out),
        vec![vec![0, 0, common::A4.0, common::A4.1]]
    );
}

#[test]
fn the_replacement_image_is_still_a_valid_jpeg() {
    let workspace = Workspace::new();
    let input = oversampled(&workspace, "in.pdf");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();
    doc.save(&out).unwrap();

    let reloaded = lopdf::Document::load(&out).unwrap();
    let stream = reloaded
        .objects
        .values()
        .find_map(|object| match object {
            lopdf::Object::Stream(stream)
                if stream.dict.get(b"Subtype").and_then(|s| s.as_name()).ok()
                    == Some(b"Image".as_slice()) =>
            {
                Some(stream)
            }
            _ => None,
        })
        .expect("the image should still be there");

    assert_eq!(
        stream.dict.get(b"Filter").unwrap().as_name().unwrap(),
        b"DCTDecode"
    );

    let decoded =
        image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg).unwrap();
    assert_eq!(
        decoded.width(),
        stream.dict.get(b"Width").unwrap().as_i64().unwrap() as u32
    );
}

#[test]
fn a_rotated_placement_is_measured_by_its_edge_lengths() {
    // The image is drawn rotated 90 degrees into a 100 point box. Measuring the
    // matrix naively by its diagonal would read the box as zero-sized and skip
    // the image entirely.
    let workspace = Workspace::new();

    let mut doc = common::build_with_image(1000, 1000, ImagePlacement::drawn(100.0, 100.0));
    let content_id = *doc
        .objects
        .iter()
        .find(|(_, object)| {
            matches!(object, lopdf::Object::Stream(stream)
            if stream.dict.get(b"Subtype").is_err())
        })
        .map(|(id, _)| id)
        .unwrap();

    let rotated = lopdf::content::Content {
        operations: vec![
            lopdf::content::Operation::new("q", vec![]),
            // [0 100 -100 0 e f]: a quarter turn with 100 point edges.
            lopdf::content::Operation::new(
                "cm",
                vec![
                    0.into(),
                    100.into(),
                    (-100).into(),
                    0.into(),
                    200.into(),
                    200.into(),
                ],
            ),
            lopdf::content::Operation::new("Do", vec![lopdf::Object::Name(b"Im1".to_vec())]),
            lopdf::content::Operation::new("Q", vec![]),
        ],
    };
    if let Ok(lopdf::Object::Stream(stream)) = doc.get_object_mut(content_id) {
        stream.set_content(rotated.encode().unwrap());
    }

    let input = workspace.write("rotated.pdf", doc);

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();

    assert_eq!(
        report.images_recompressed, 1,
        "a rotated image should still be measured, skips: {:?}",
        report.skipped
    );
}

/// A 1000 pixel photo-like image, deflated after a predictor, drawn into a
/// 100 point box: 720 dpi, so there is plenty to take away.
fn predicted(workspace: &Workspace, name: &str, predictor: Predictor) -> std::path::PathBuf {
    workspace.write(
        name,
        common::build_with_predicted_image(
            1000,
            1000,
            ImagePlacement::drawn(100.0, 100.0),
            predictor,
        ),
    )
}

/// The only image stream in a saved document.
fn only_image(path: &std::path::Path) -> lopdf::Stream {
    let doc = lopdf::Document::load(path).unwrap();
    let mut images: Vec<lopdf::Stream> = doc
        .objects
        .into_values()
        .filter_map(|object| match object {
            lopdf::Object::Stream(stream)
                if stream.dict.get(b"Subtype").and_then(|s| s.as_name()).ok()
                    == Some(b"Image".as_slice()) =>
            {
                Some(stream)
            }
            _ => None,
        })
        .collect();
    assert_eq!(images.len(), 1, "expected exactly one image");
    images.pop().unwrap()
}

/// Mean absolute difference per sample between two equally sized buffers.
fn mean_difference(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| u64::from(x.abs_diff(y)))
        .sum();
    total as f64 / a.len() as f64
}

/// Compress a predicted image and check that it shrank, was resampled, and
/// still shows the same picture.
fn check_predicted_image_is_recompressed(predictor: Predictor) {
    let workspace = Workspace::new();
    let input = predicted(&workspace, "in.pdf", predictor);
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();
    doc.save(&out).unwrap();

    assert_eq!(
        report.images_recompressed, 1,
        "the predicted image should be recompressed, skips: {:?}",
        report.skipped
    );

    let before = std::fs::metadata(&input).unwrap().len();
    let after = std::fs::metadata(&out).unwrap().len();
    assert!(
        after * 2 < before,
        "{after} should be well under half of {before}"
    );

    // Still opens, still one page.
    assert_eq!(Document::open(&out).unwrap().page_count(), 1);

    // 720 dpi down to 72 is a tenth, so roughly 100 pixels across.
    let stream = only_image(&out);
    assert_eq!(
        stream.dict.get(b"Filter").unwrap().as_name().unwrap(),
        b"DCTDecode"
    );
    assert!(stream.dict.get(b"DecodeParms").is_err());

    let decoded = image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg)
        .unwrap()
        .to_rgb8();
    let (width, height) = decoded.dimensions();
    assert!(
        (95..=105).contains(&width),
        "expected about 100 pixels, got {width}"
    );
    assert_eq!(width, height, "aspect ratio should be preserved");
    assert_eq!(
        i64::from(width),
        stream.dict.get(b"Width").unwrap().as_i64().unwrap()
    );

    // The picture must survive: a predictor reversed wrongly smears errors
    // down and across the image, leaving it nothing like the original.
    let expected = image::imageops::resize(
        &common::soft_photo_pixels(1000, 1000),
        width,
        height,
        image::imageops::Lanczos3,
    );
    let difference = mean_difference(expected.as_raw(), decoded.as_raw());
    assert!(
        difference < 4.0,
        "recompressed image differs from the original by {difference:.1} per sample"
    );
}

#[test]
fn a_png_predicted_image_is_recompressed() {
    check_predicted_image_is_recompressed(Predictor::Png);
}

#[test]
fn a_tiff_predicted_image_is_recompressed() {
    check_predicted_image_is_recompressed(Predictor::Tiff);
}

#[test]
fn an_unknown_predictor_is_skipped_rather_than_misread() {
    let workspace = Workspace::new();

    let mut doc = common::build_with_predicted_image(
        1000,
        1000,
        ImagePlacement::drawn(100.0, 100.0),
        Predictor::Png,
    );
    for object in doc.objects.values_mut() {
        if let lopdf::Object::Stream(stream) = object {
            if let Ok(lopdf::Object::Dictionary(parms)) = stream.dict.get_mut(b"DecodeParms") {
                parms.set("Predictor", lopdf::Object::Integer(7));
            }
        }
    }
    let input = workspace.write("in.pdf", doc);

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::compress(&mut doc, CompressLevel::Screen).unwrap();

    assert_eq!(report.images_recompressed, 0);
    assert_eq!(report.skipped.get(&SkipReason::UnsupportedFilter), Some(&1));
}

/// MuPDF reverses the predictor itself when it renders the original, so the
/// original and the compressed page should look alike where the image is.
#[cfg(feature = "render")]
#[test]
fn a_compressed_predicted_image_renders_like_the_original() {
    use pdf_core::render::{PageRenderer, Scale};

    let workspace = Workspace::new();
    let input = predicted(&workspace, "in.pdf", Predictor::Png);
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::compress(&mut doc, CompressLevel::Ebook).unwrap();
    doc.save(&out).unwrap();

    let original = PageRenderer::open(&input)
        .unwrap()
        .render(0, Scale::Dpi(72.0))
        .unwrap();
    let compressed = PageRenderer::open(&out)
        .unwrap()
        .render(0, Scale::Dpi(72.0))
        .unwrap();
    assert_eq!(
        (original.width, original.height),
        (compressed.width, compressed.height)
    );

    // The image sits at (50, 100) in PDF space, 100 points square; at 72 dpi
    // and with y flipped, that is columns 50 to 150 and rows 642 to 742. Stay
    // a couple of pixels inside to keep clear of antialiased edges.
    let region = |page: &pdf_core::RenderedPage| -> Vec<u8> {
        let stride = page.stride();
        let channels = page.channels as usize;
        (644..740)
            .flat_map(|row| {
                let start = row * stride + 52 * channels;
                page.pixels[start..start + 96 * channels].to_vec()
            })
            .collect()
    };
    let (before, after) = (region(&original), region(&compressed));

    assert!(
        after.iter().any(|&sample| sample != 255),
        "the image area rendered blank"
    );
    let difference = mean_difference(&before, &after);
    assert!(
        difference < 4.0,
        "rendered image differs from the original by {difference:.1} per sample"
    );
}
