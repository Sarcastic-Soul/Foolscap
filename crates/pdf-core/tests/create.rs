//! Blank-document creation.

mod common;

use common::Workspace;
use pdf_core::{create_blank, Document, PageSize};

#[test]
fn five_a4_pages_save_and_reopen() {
    let workspace = Workspace::new();
    let path = workspace.join("blank.pdf");

    let mut doc = create_blank(5, PageSize::A4).unwrap();
    assert_eq!(doc.page_count(), 5);
    doc.save(&path).unwrap();

    let reloaded = Document::open(&path).unwrap();
    assert_eq!(reloaded.page_count(), 5);

    let boxes = common::page_media_boxes(&path);
    assert_eq!(boxes.len(), 5);
    for media in boxes {
        assert_eq!(media, vec![0, 0, 595, 842]);
    }
}

#[test]
fn a_custom_size_round_trips() {
    let workspace = Workspace::new();
    let path = workspace.join("tall.pdf");

    let size = PageSize::parse("300x600").unwrap();
    let mut doc = create_blank(2, size).unwrap();
    doc.save(&path).unwrap();

    let reloaded = Document::open(&path).unwrap();
    assert_eq!(reloaded.page_count(), 2);
    for media in common::page_media_boxes(&path) {
        assert_eq!(media, vec![0, 0, 300, 600]);
    }
}

#[test]
fn every_page_is_empty() {
    let workspace = Workspace::new();
    let path = workspace.join("empty.pdf");

    let mut doc = create_blank(3, PageSize::Letter).unwrap();
    doc.save(&path).unwrap();

    let loaded = lopdf::Document::load(&path).unwrap();
    for page_id in loaded.get_pages().into_values() {
        let content = loaded.get_page_content(page_id);
        // The content stream exists but draws nothing.
        assert!(
            content.iter().all(|byte| byte.is_ascii_whitespace()),
            "a blank page draws nothing"
        );
    }
}

#[test]
fn zero_pages_are_refused() {
    let error = create_blank(0, PageSize::A4).unwrap_err();
    assert!(matches!(error, pdf_core::PdfError::EmptySelection));
}
