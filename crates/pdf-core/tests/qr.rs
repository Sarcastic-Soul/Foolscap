//! QR generation tests. Only built with the `convert` feature.

#![cfg(feature = "convert")]

mod common;

use lopdf::content::Content;
use lopdf::{Document as LopdfDocument, Object};
use pdf_core::{qr_code, QrEcc, QrOptions};

use common::Workspace;

const URL: &str = "https://example.com/foolscap";
/// Millimetres to points.
const MM_TO_PT: f64 = 72.0 / 25.4;
/// The quiet zone drawn around the symbol, in modules.
const QUIET_ZONE: f64 = 4.0;

#[test]
fn a_url_encodes_into_a_one_page_vector_document() {
    let options = QrOptions {
        size_mm: 50.0,
        ecc: QrEcc::Medium,
        label: None,
    };
    let mut doc = qr_code(URL, &options).expect("a short URL always encodes");

    let workspace = Workspace::new();
    let path = workspace.join("qr.pdf");
    doc.save(&path).expect("could not save the document");

    let reloaded = LopdfDocument::load(&path).expect("could not reload the saved file");
    assert_eq!(reloaded.get_pages().len(), 1, "the code is one page");

    // The page is the requested square: 50 mm to a side, in points.
    let size_pt = 50.0 * MM_TO_PT;
    let boxes = common::page_media_boxes(&path);
    assert_eq!(boxes.len(), 1);
    let [x0, y0, x1, y1] = [boxes[0][0], boxes[0][1], boxes[0][2], boxes[0][3]];
    assert_eq!((x0, y0), (0, 0));
    assert!(
        (x1 - size_pt.round() as i64).abs() <= 1,
        "width {x1} should be about {size_pt:.1} points"
    );
    assert_eq!(x1, y1, "without a label the page is square");

    // The content is a stack of `re` fills, and the dark modules in it are
    // exactly the ones qrcodegen's own matrix holds for the same input.
    let rects = content_rects(&reloaded);
    assert!(!rects.is_empty(), "the content stream draws rectangles");

    let expected =
        qrcodegen::QrCode::encode_text(URL, qrcodegen::QrCodeEcc::Medium).expect("encodes");
    let size = expected.size() as f64;
    let module_pt = size_pt / (size + 2.0 * QUIET_ZONE);

    // First rectangle is the white background: the whole page.
    assert_eq!(rects[0][2], rects[0][3], "the background is square");
    assert!((rects[0][2] as f64 - size_pt).abs() < 1.0);

    let quiet = QUIET_ZONE * module_pt;
    let mut drawn = Vec::new();
    for [x, y, w, _h] in rects.iter().skip(1) {
        assert!(
            (*w as f64 - module_pt).abs() < 1e-3,
            "every module is one module wide"
        );
        let column = ((*x as f64 - quiet) / module_pt).round() as i64;
        let row_top_down = size as i64 - 1 - ((*y as f64 - quiet) / module_pt).round() as i64;
        drawn.push((column, row_top_down));
    }

    let mut wanted = Vec::new();
    for y in 0..expected.size() {
        for x in 0..expected.size() {
            if expected.get_module(x, y) {
                wanted.push((x as i64, y as i64));
            }
        }
    }

    assert_eq!(drawn, wanted, "the drawn matrix matches qrcodegen's");
}

#[test]
fn a_label_widens_the_page_and_brings_a_font() {
    let options = QrOptions {
        size_mm: 40.0,
        ecc: QrEcc::High,
        label: Some("Scan me".to_string()),
    };
    let mut doc = qr_code(URL, &options).expect("a short URL always encodes");

    let workspace = Workspace::new();
    let path = workspace.join("qr-labelled.pdf");
    doc.save(&path).expect("could not save the document");

    let reloaded = LopdfDocument::load(&path).expect("could not reload the saved file");
    let page_id = reloaded.get_pages().into_values().next().unwrap();
    let page = reloaded.get_dictionary(page_id).expect("the page exists");

    // The label strip makes the page taller than it is wide.
    let boxes = common::page_media_boxes(&path);
    assert!(
        boxes[0][3] > boxes[0][2],
        "a label adds height: {:?}",
        boxes[0]
    );

    // A Base14 font is in the page resources, and the label is drawn with it.
    let resources = match page.get(b"Resources").expect("the page has resources") {
        Object::Dictionary(dict) => dict.clone(),
        Object::Reference(id) => reloaded
            .get_dictionary(*id)
            .expect("resource exists")
            .clone(),
        other => panic!("unexpected resources: {other:?}"),
    };
    let fonts = resources
        .get(b"Font")
        .and_then(Object::as_dict)
        .expect("the label needs a font");
    let font_id = fonts
        .get(b"F1")
        .and_then(Object::as_reference)
        .expect("the font is named F1");
    let font = reloaded.get_dictionary(font_id).expect("the font exists");
    assert_eq!(
        font.get(b"BaseFont").and_then(Object::as_name).ok(),
        Some(b"Helvetica".as_slice())
    );

    let text = shown_text(&reloaded);
    assert_eq!(text, vec!["Scan me".to_string()]);
}

#[test]
fn every_ecc_level_produces_a_document() {
    for (ecc, qrcodegen_ecc) in [
        (QrEcc::Low, qrcodegen::QrCodeEcc::Low),
        (QrEcc::Medium, qrcodegen::QrCodeEcc::Medium),
        (QrEcc::Quartile, qrcodegen::QrCodeEcc::Quartile),
        (QrEcc::High, qrcodegen::QrCodeEcc::High),
    ] {
        let options = QrOptions {
            size_mm: 30.0,
            ecc,
            label: None,
        };
        let mut doc = qr_code("ecc probe", &options)
            .unwrap_or_else(|error| panic!("{ecc:?} should encode: {error}"));

        let workspace = Workspace::new();
        let path = workspace.join("qr-ecc.pdf");
        doc.save(&path).expect("could not save the document");
        let reloaded = LopdfDocument::load(&path).expect("could not reload the saved file");

        // Higher correction means more codewords, so for a fixed input the
        // modules drawn must match the level's own matrix — including a
        // different count between levels when the version steps up.
        let expected = qrcodegen::QrCode::encode_text("ecc probe", qrcodegen_ecc).expect("encodes");
        let rects = content_rects(&reloaded);
        let dark_count: usize = rects.len() - 1; // minus the background
        let wanted: usize = (0..expected.size())
            .map(|y| {
                (0..expected.size())
                    .filter(|x| expected.get_module(*x, y))
                    .count()
            })
            .sum();
        assert_eq!(dark_count, wanted, "{ecc:?} drew the wrong module count");
    }
}

/// Every `re` rectangle in the page's content stream, as `[x, y, w, h]`.
fn content_rects(doc: &LopdfDocument) -> Vec<[f32; 4]> {
    let page_id = doc.get_pages().into_values().next().unwrap();
    let content = doc.get_page_content(page_id);
    let decoded = Content::decode(&content).expect("the content stream decodes");

    decoded
        .operations
        .iter()
        .filter(|operation| operation.operator == "re")
        .map(|operation| {
            let number = |operand: &Object| -> f32 {
                match operand {
                    Object::Integer(value) => *value as f32,
                    Object::Real(value) => *value,
                    other => panic!("a rectangle operand should be a number, got {other:?}"),
                }
            };
            [
                number(&operation.operands[0]),
                number(&operation.operands[1]),
                number(&operation.operands[2]),
                number(&operation.operands[3]),
            ]
        })
        .collect()
}

/// The text the page's content stream draws with `Tj`.
fn shown_text(doc: &LopdfDocument) -> Vec<String> {
    let page_id = doc.get_pages().into_values().next().unwrap();
    let content = doc.get_page_content(page_id);
    let decoded = Content::decode(&content).expect("the content stream decodes");

    decoded
        .operations
        .iter()
        .filter(|operation| operation.operator == "Tj")
        .filter_map(|operation| operation.operands.first())
        .filter_map(|operand| match operand {
            Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .collect()
}
