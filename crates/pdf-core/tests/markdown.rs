//! Markdown extraction tests. Only built with the `render` feature.

#![cfg(feature = "render")]

mod common;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document as LopdfDocument, Object, Stream};
use pdf_core::{to_markdown, Document, MarkdownOptions, PageRange};

use common::Workspace;

/// A4 in points, as the common builders use it.
const A4: (i64, i64) = common::A4;

/// Text as the bytes a WinAnsiEncoding font expects: ASCII passes through and
/// the bullet glyph — the one non-ASCII character these fixtures use — is the
/// 0x95 cell.
fn winansi(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c {
            '\u{2022}' => 0x95,
            other => other as u8,
        })
        .collect()
}

/// A document with one page per group of lines. Each line is `(x, text,
/// size)`: drawn at `x` points from the left edge, at that font size, stacked
/// down the page. Built the way [`common::build_text_page`] builds its single
/// line, so what MuPDF reads back matches the other fixtures.
fn pages(line_groups: &[&[(i64, &str, f32)]]) -> LopdfDocument {
    let mut doc = LopdfDocument::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut kids = Vec::new();
    for lines in line_groups {
        let lines = *lines;
        let mut operations = Vec::new();
        let mut y = 720;
        for (x, text, size) in lines {
            operations.push(Operation::new("BT", vec![]));
            operations.push(Operation::new("Tf", vec!["F1".into(), (*size).into()]));
            operations.push(Operation::new("Td", vec![(*x).into(), y.into()]));
            operations.push(Operation::new(
                "Tj",
                vec![Object::string_literal(winansi(text))],
            ));
            operations.push(Operation::new("ET", vec![]));
            y -= (size * 2.5) as i64;
        }
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations }
                .encode()
                .expect("fixture content encodes"),
        ));

        kids.push(Object::Reference(doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), A4.0.into(), A4.1.into()],
        })));
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => line_groups.len() as i64,
            "Kids" => kids,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    doc
}

/// Build a fixture, write it, and open it as the document `to_markdown`
/// reads.
fn fixture(workspace: &Workspace, name: &str, line_groups: &[&[(i64, &str, f32)]]) -> Document {
    let path = workspace.write(name, pages(line_groups));
    Document::open(&path).expect("the fixture opens")
}

#[test]
fn font_sizes_become_headings_and_bullets_become_items() {
    let workspace = Workspace::new();
    let doc = fixture(
        &workspace,
        "mixed.pdf",
        &[&[
            (72, "Quarterly Report", 36.0),
            (72, "Prepared by the crew", 24.0),
            (72, "The quarter went well, all things considered.", 12.0),
            (72, "\u{2022} Reconcile the ledger", 12.0),
            (72, "\u{2022} File the paperwork", 12.0),
        ]],
    );

    let markdown = to_markdown(&doc, &MarkdownOptions::default()).expect("extracts");

    assert!(
        markdown.contains("# Quarterly Report"),
        "the largest size is level one:\n{markdown}"
    );
    assert!(
        markdown.contains("## Prepared by the crew"),
        "the next size is level two:\n{markdown}"
    );
    assert!(
        markdown.contains("The quarter went well, all things considered."),
        "body text is a paragraph:\n{markdown}"
    );
    assert!(
        markdown.contains("- Reconcile the ledger\n- File the paperwork"),
        "bullet lines become items:\n{markdown}"
    );
    assert!(
        !markdown.contains("### "),
        "no deeper level was manufactured:\n{markdown}"
    );
}

#[test]
fn a_page_range_selects_pages_without_markers() {
    let workspace = Workspace::new();
    let doc = fixture(
        &workspace,
        "three.pdf",
        &[
            &[(72, "Alpha opening", 12.0)],
            &[(72, "Beta middle", 12.0)],
            &[(72, "Gamma close", 12.0)],
        ],
    );

    let options = MarkdownOptions {
        pages: Some(PageRange::parse("2-3").expect("valid range")),
        ..MarkdownOptions::default()
    };
    let markdown = to_markdown(&doc, &options).expect("extracts");

    assert!(markdown.contains("Beta middle"), "{markdown}");
    assert!(markdown.contains("Gamma close"), "{markdown}");
    assert!(!markdown.contains("Alpha"), "{markdown}");
    assert!(
        !markdown.contains("Page"),
        "pages contribute no markers of their own:\n{markdown}"
    );

    // Without a range, every page is read.
    let all = to_markdown(&doc, &MarkdownOptions::default()).expect("extracts");
    assert!(all.contains("Alpha opening") && all.contains("Gamma close"));
}

#[test]
fn detection_can_be_switched_off() {
    let workspace = Workspace::new();
    let doc = fixture(
        &workspace,
        "plain.pdf",
        &[&[
            (72, "A Plain Title", 36.0),
            (72, "\u{2022} an item", 12.0),
            (72, "Plain body text.", 12.0),
        ]],
    );

    let options = MarkdownOptions {
        headings: false,
        lists: false,
        ..MarkdownOptions::default()
    };
    let markdown = to_markdown(&doc, &options).expect("extracts");

    assert!(
        !markdown.contains("# "),
        "no headings without the option:\n{markdown}"
    );
    assert!(!markdown.contains("- "), "no items either:\n{markdown}");
    assert!(
        markdown.contains("A Plain Title") && markdown.contains("Plain body text."),
        "the text survives:\n{markdown}"
    );
    assert!(
        markdown.contains('\u{2022}'),
        "the bullet glyph is left as it was drawn:\n{markdown}"
    );
}

#[test]
fn an_indented_line_is_an_item_even_without_a_glyph() {
    let workspace = Workspace::new();
    let doc = fixture(
        &workspace,
        "indented.pdf",
        &[&[
            (72, "A lead-in paragraph.", 12.0),
            (96, "an indented item", 12.0),
        ]],
    );

    let markdown = to_markdown(&doc, &MarkdownOptions::default()).expect("extracts");

    assert!(
        markdown.contains("- an indented item"),
        "indentation alone marks a list item:\n{markdown}"
    );
}

#[test]
fn pages_are_separated_by_a_blank_line() {
    let workspace = Workspace::new();
    let doc = fixture(
        &workspace,
        "twopages.pdf",
        &[
            &[(72, "First page body.", 12.0)],
            &[(72, "Second page body.", 12.0)],
        ],
    );

    let markdown = to_markdown(&doc, &MarkdownOptions::default()).expect("extracts");

    assert!(
        markdown.contains("First page body.\n\nSecond page body."),
        "one blank line between the pages:\n{markdown:?}"
    );
}
