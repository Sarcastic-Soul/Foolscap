//! Halving pages: geometry, order, and the inheritance trap.

mod common;

use common::{build, Attributes, Workspace, A4};
use pdf_core::{halve, Document, HalveDirection, HalveOptions};
use std::path::Path;

/// A4 turned on its side, the shape of a scanned two-up spread.
const LANDSCAPE: (i64, i64) = (842, 595);

fn halved(path: &Path, direction: HalveDirection, gap: f64, swap: bool) -> Vec<[f64; 4]> {
    let workspace = Workspace::new();
    let out = workspace.join("halved.pdf");

    let mut doc = Document::open_with(path, None).unwrap();
    halve(
        &mut doc,
        &HalveOptions {
            direction,
            gap,
            swap,
        },
    )
    .unwrap();
    doc.save(&out).unwrap();

    crop_boxes(&out)
}

/// The box each page shows, from the reopened file: `/CropBox` where the page
/// has one, otherwise its `/MediaBox`.
fn crop_boxes(path: &Path) -> Vec<[f64; 4]> {
    let doc = lopdf::Document::load(path).unwrap();
    doc.get_pages()
        .into_values()
        .map(|page_id| {
            let dict = doc.get_dictionary(page_id).unwrap();
            let object = dict
                .get(b"CropBox")
                .or_else(|_| dict.get(b"MediaBox"))
                .expect("every halved page carries a box");
            let (_, resolved) = doc.dereference(object).unwrap();
            let values = resolved.as_array().unwrap();
            let number = |index: usize| match &values[index] {
                lopdf::Object::Integer(value) => *value as f64,
                lopdf::Object::Real(value) => f64::from(*value),
                other => panic!("a box entry that is not a number: {other:?}"),
            };
            [number(0), number(1), number(2), number(3)]
        })
        .collect()
}

#[test]
fn a_landscape_page_becomes_two_portrait_halves() {
    let workspace = Workspace::new();
    let path = workspace.write(
        "spread.pdf",
        build(1, "Spread", LANDSCAPE, Attributes::OnPage),
    );

    let boxes = halved(&path, HalveDirection::Vertical, 0.0, false);

    assert_eq!(boxes.len(), 2, "one page doubles");
    assert_eq!(
        boxes[0],
        [0.0, 0.0, 421.0, 595.0],
        "the left half comes first"
    );
    assert_eq!(boxes[1], [421.0, 0.0, 842.0, 595.0], "then the right");
}

#[test]
fn every_page_doubles_and_keeps_its_identity() {
    let workspace = Workspace::new();
    let path = workspace.write("two.pdf", build(2, "Page", LANDSCAPE, Attributes::OnPage));

    let out = workspace.join("doubled.pdf");
    let mut doc = Document::open_with(&path, None).unwrap();
    assert_eq!(doc.page_count(), 2);
    halve(
        &mut doc,
        &HalveOptions {
            direction: HalveDirection::Vertical,
            gap: 0.0,
            swap: false,
        },
    )
    .unwrap();
    assert_eq!(doc.page_count(), 4);
    doc.save(&out).unwrap();

    let labels = common::page_labels(&out);
    assert_eq!(labels, vec!["Page 1", "Page 1", "Page 2", "Page 2"]);
    assert_eq!(crop_boxes(&out).len(), 4);
}

#[test]
fn a_gap_shrinks_both_halves_at_the_cut_line() {
    let workspace = Workspace::new();
    let path = workspace.write(
        "gutter.pdf",
        build(1, "Spread", LANDSCAPE, Attributes::OnPage),
    );

    let boxes = halved(&path, HalveDirection::Vertical, 12.0, false);

    assert_eq!(boxes[0], [0.0, 0.0, 409.0, 595.0]);
    assert_eq!(boxes[1], [433.0, 0.0, 842.0, 595.0]);
}

#[test]
fn a_gap_that_eats_a_half_leaves_the_document_untouched() {
    let workspace = Workspace::new();
    let path = workspace.write("thin.pdf", build(1, "Thin", (100, 100), Attributes::OnPage));

    let mut doc = Document::open_with(&path, None).unwrap();
    let error = halve(
        &mut doc,
        &HalveOptions {
            direction: HalveDirection::Vertical,
            gap: 60.0,
            swap: false,
        },
    )
    .unwrap_err();
    assert!(matches!(error, pdf_core::PdfError::InvalidCropBox { .. }));
    assert_eq!(
        doc.page_count(),
        1,
        "the source document survives the refusal"
    );
}

#[test]
fn swap_reverses_the_reading_order() {
    let workspace = Workspace::new();
    let path = workspace.write("rtl.pdf", build(1, "Spread", LANDSCAPE, Attributes::OnPage));

    let boxes = halved(&path, HalveDirection::Vertical, 0.0, true);

    assert_eq!(
        boxes[0],
        [421.0, 0.0, 842.0, 595.0],
        "the right half comes first"
    );
    assert_eq!(boxes[1], [0.0, 0.0, 421.0, 595.0]);
}

#[test]
fn a_horizontal_cut_reads_top_then_bottom() {
    let workspace = Workspace::new();
    let path = workspace.write(
        "stacked.pdf",
        build(1, "Stack", (595, 842), Attributes::OnPage),
    );

    let boxes = halved(&path, HalveDirection::Horizontal, 0.0, false);

    assert_eq!(
        boxes[0],
        [0.0, 421.0, 595.0, 842.0],
        "the top half comes first"
    );
    assert_eq!(boxes[1], [0.0, 0.0, 595.0, 421.0]);
}

#[test]
fn an_inherited_media_box_still_halves() {
    // The /MediaBox lives on the /Pages node, inherited by every page — the
    // case that breaks code which reads boxes off the page alone.
    let workspace = Workspace::new();
    let path = workspace.write(
        "inherited.pdf",
        build(2, "Spread", LANDSCAPE, Attributes::Inherited),
    );

    let out = workspace.join("halved.pdf");
    let mut doc = Document::open_with(&path, None).unwrap();
    halve(
        &mut doc,
        &HalveOptions {
            direction: HalveDirection::Vertical,
            gap: 0.0,
            swap: false,
        },
    )
    .unwrap();
    doc.save(&out).unwrap();

    assert_eq!(crop_boxes(&out).len(), 4);
    for (index, half) in crop_boxes(&out).into_iter().enumerate() {
        let side = index % 2;
        let expected_x0 = [0.0, 421.0][side];
        let expected_x1 = [421.0, 842.0][side];
        assert_eq!(half, [expected_x0, 0.0, expected_x1, 595.0]);
    }
}

#[test]
fn an_existing_crop_box_is_what_gets_halved() {
    // A page already cropped to its spread shows only that much after halving.
    let workspace = Workspace::new();
    let path = workspace.write("cropped.pdf", build(1, "Spread", A4, Attributes::OnPage));
    let mut doc = Document::open_with(&path, None).unwrap();
    pdf_core::crop(
        &mut doc,
        &pdf_core::PageRange::All,
        pdf_core::Rect::from_xywh(0.0, 0.0, 500.0, 400.0).unwrap(),
    )
    .unwrap();
    doc.save(&path).unwrap();

    let boxes = halved(&path, HalveDirection::Vertical, 0.0, false);
    assert_eq!(boxes[0], [0.0, 0.0, 250.0, 400.0]);
    assert_eq!(boxes[1], [250.0, 0.0, 500.0, 400.0]);
}
