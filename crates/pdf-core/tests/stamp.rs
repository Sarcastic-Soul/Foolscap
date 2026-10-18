//! End-to-end tests for the stamp engine.
//!
//! Fixtures are generated per test so that page geometry and attribute
//! placement can be varied: whether `/MediaBox` and `/Resources` sit on the
//! page or are inherited is where reassembly bugs hide.

mod common;

use common::{Attributes, Workspace, A4, LETTER};
use lopdf::content::Content;
use lopdf::Object;
use pdf_core::{Document, PageRange, PdfError, StampContent, StampLayer, StampOptions};

fn range(spec: &str) -> PageRange {
    PageRange::parse(spec).expect("test range should parse")
}

/// Every literal drawn with `Tj` on each page, in page order.
fn page_texts(path: &std::path::Path) -> Vec<Vec<String>> {
    let doc = lopdf::Document::load(path).expect("could not reload document");
    doc.get_pages()
        .into_values()
        .map(|page_id| {
            let content = doc.get_page_content(page_id);
            let decoded = Content::decode(&content).expect("could not decode content");
            decoded
                .operations
                .iter()
                .filter(|op| op.operator == "Tj")
                .filter_map(|op| op.operands.first())
                .filter_map(|operand| match operand {
                    Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

fn text_opts(text: &str) -> StampOptions {
    StampOptions {
        content: StampContent::Text(text.to_string()),
        ..StampOptions::default()
    }
}

#[test]
fn page_numbers_expand_per_page() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("all"), &text_opts("Page {n} of {total}")).unwrap();
    doc.save(&out).unwrap();

    let texts = page_texts(&out);
    assert_eq!(texts.len(), 3);
    assert!(texts[0].iter().any(|t| t == "Page 1 of 3"), "got {texts:?}");
    assert!(texts[1].iter().any(|t| t == "Page 2 of 3"), "got {texts:?}");
    assert!(texts[2].iter().any(|t| t == "Page 3 of 3"), "got {texts:?}");
    // The original labels survive alongside the stamp.
    assert!(texts[0].iter().any(|t| t == "Page 1"), "got {texts:?}");
}

#[test]
fn stamping_touches_only_the_selected_pages() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("2"), &text_opts("STAMP")).unwrap();
    doc.save(&out).unwrap();

    let texts = page_texts(&out);
    assert!(!texts[0].iter().any(|t| t == "STAMP"), "got {texts:?}");
    assert!(texts[1].iter().any(|t| t == "STAMP"), "got {texts:?}");
    assert!(!texts[2].iter().any(|t| t == "STAMP"), "got {texts:?}");
}

#[test]
fn selecting_a_page_twice_stamps_it_once() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 2, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("1,1"), &text_opts("ONCE")).unwrap();
    doc.save(&out).unwrap();

    let texts = page_texts(&out);
    let count = texts[0].iter().filter(|t| *t == "ONCE").count();
    assert_eq!(count, 1, "got {texts:?}");
}

#[test]
fn stamping_an_inherited_document_keeps_working() {
    let workspace = Workspace::new();
    let input = workspace.write(
        "letter.pdf",
        common::build(2, "Page", LETTER, Attributes::Inherited),
    );
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("all"), &text_opts("HI")).unwrap();
    doc.save(&out).unwrap();

    let texts = page_texts(&out);
    assert!(texts.iter().all(|t| t.iter().any(|s| s == "HI")));
    assert_eq!(
        common::page_media_boxes(&out),
        vec![
            vec![0, 0, LETTER.0, LETTER.1],
            vec![0, 0, LETTER.0, LETTER.1]
        ]
    );
}

#[test]
fn stamping_a_mixed_size_document_preserves_geometry() {
    let workspace = Workspace::new();
    let a = workspace.write("a4.pdf", common::build(1, "A", A4, Attributes::OnPage));
    let b = workspace.write(
        "letter.pdf",
        common::build(1, "B", LETTER, Attributes::Inherited),
    );
    let merged = workspace.join("merged.pdf");
    pdf_core::merge(&[a, b], &merged).unwrap();

    let out = workspace.join("out.pdf");
    let mut doc = Document::open(&merged).unwrap();
    pdf_core::stamp(&mut doc, &range("all"), &text_opts("{n}/{total}")).unwrap();
    doc.save(&out).unwrap();

    assert_eq!(
        common::page_media_boxes(&out),
        vec![vec![0, 0, A4.0, A4.1], vec![0, 0, LETTER.0, LETTER.1]]
    );
    let texts = page_texts(&out);
    assert!(texts[0].iter().any(|t| t == "1/2"), "got {texts:?}");
    assert!(texts[1].iter().any(|t| t == "2/2"), "got {texts:?}");
}

#[test]
fn translucent_stamps_add_an_opacity_state() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let out = workspace.join("out.pdf");

    let mut opts = text_opts("FAINT");
    opts.opacity = 0.25;
    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("all"), &opts).unwrap();
    doc.save(&out).unwrap();

    let doc = lopdf::Document::load(&out).unwrap();
    let page_id = doc.get_pages().into_values().next().unwrap();
    let resources = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .clone();
    let states = match &resources {
        Object::Reference(id) => doc
            .get_dictionary(*id)
            .unwrap()
            .get(b"ExtGState")
            .unwrap()
            .clone(),
        Object::Dictionary(dict) => dict.get(b"ExtGState").unwrap().clone(),
        _ => panic!("unexpected resources"),
    };
    let dict = match &states {
        Object::Reference(id) => doc.get_dictionary(*id).unwrap().clone(),
        Object::Dictionary(dict) => dict.clone(),
        _ => panic!("unexpected ExtGState"),
    };
    let gs = match dict.get(b"GsA").unwrap() {
        Object::Reference(id) => doc.get_dictionary(*id).unwrap().clone(),
        _ => panic!("expected the stamp graphics state"),
    };
    let ca = match gs.get(b"ca").unwrap() {
        Object::Real(v) => *v,
        Object::Integer(v) => *v as f32,
        other => panic!("unexpected ca {other:?}"),
    };
    assert!((ca - 0.25).abs() < 1e-6, "got {ca}");
}

#[test]
fn opaque_stamps_need_no_graphics_state() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("all"), &text_opts("SOLID")).unwrap();
    doc.save(&out).unwrap();

    let bytes = std::fs::read(&out).unwrap();
    assert!(
        !bytes.windows(4).any(|w| w == b"/GsA"),
        "an opaque stamp should not add an ExtGState"
    );
}

#[test]
fn under_stamps_draw_beneath_the_page() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let over = workspace.join("over.pdf");
    let under = workspace.join("under.pdf");

    let mut opts = text_opts("MARK");
    opts.layer = StampLayer::Over;
    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("all"), &opts).unwrap();
    doc.save(&over).unwrap();

    let mut opts = text_opts("MARK");
    opts.layer = StampLayer::Under;
    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("all"), &opts).unwrap();
    doc.save(&under).unwrap();

    // Contents order decides paint order: last draws on top.
    let order_of = |path: &std::path::Path| {
        let doc = lopdf::Document::load(path).unwrap();
        let page_id = doc.get_pages().into_values().next().unwrap();
        let dict = doc.get_dictionary(page_id).unwrap().clone();
        match dict.get(b"Contents").unwrap() {
            Object::Array(items) => items.len(),
            Object::Reference(_) => 1,
            _ => 0,
        }
    };
    assert_eq!(order_of(&over), 2);
    assert_eq!(order_of(&under), 2);

    // The stamp stream is last for Over and first for Under.
    let contents_ids = |path: &std::path::Path| {
        let doc = lopdf::Document::load(path).unwrap();
        let page_id = doc.get_pages().into_values().next().unwrap();
        let dict = doc.get_dictionary(page_id).unwrap().clone();
        match dict.get(b"Contents").unwrap().clone() {
            Object::Array(items) => items
                .into_iter()
                .map(|o| o.as_reference().expect("contents should be references"))
                .collect::<Vec<_>>(),
            _ => panic!("expected a contents array"),
        }
    };
    let has_mark = |path: &std::path::Path, id: lopdf::ObjectId| {
        let doc = lopdf::Document::load(path).unwrap();
        let bytes = doc
            .get_object(id)
            .unwrap()
            .as_stream()
            .unwrap()
            .content
            .clone();
        let decoded = Content::decode(&bytes).unwrap();
        decoded.operations.iter().any(|op| {
            op.operator == "Tj"
                && op.operands.first().is_some_and(|operand| match operand {
                    Object::String(bytes, _) => bytes == b"MARK",
                    _ => false,
                })
        })
    };
    let over_ids = contents_ids(&over);
    let under_ids = contents_ids(&under);
    assert!(has_mark(&over, *over_ids.last().unwrap()));
    assert!(has_mark(&under, under_ids[0]));
}

#[test]
fn image_stamps_embed_an_xobject_drawn_per_page() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 2, "Page");
    let out = workspace.join("out.pdf");

    let png = common::jpeg_bytes(8, 8, 90);
    let mut opts = StampOptions::image(png);
    opts.opacity = 0.5;
    let mut doc = Document::open(&input).unwrap();
    pdf_core::stamp(&mut doc, &range("all"), &opts).unwrap();
    doc.save(&out).unwrap();

    let doc = lopdf::Document::load(&out).unwrap();
    let images: Vec<_> = doc
        .objects
        .values()
        .filter_map(|object| {
            let Object::Stream(stream) = object else {
                return None;
            };
            (stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                == Some(b"Image".as_slice()))
            .then_some(stream)
        })
        .collect();
    assert!(!images.is_empty(), "expected an embedded stamp image");

    for page_id in doc.get_pages().into_values() {
        let content = doc.get_page_content(page_id);
        let decoded = Content::decode(&content).unwrap();
        assert!(
            decoded.operations.iter().any(|op| op.operator == "Do"),
            "every stamped page should draw the image"
        );
    }
}

#[test]
fn stamping_reports_progress_per_page() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");

    let mut doc = Document::open(&input).unwrap();
    let mut ticks = Vec::new();
    pdf_core::stamp_with_progress(
        &mut doc,
        &range("all"),
        &text_opts("P"),
        Some(&mut |progress: pdf_core::progress::Progress| {
            ticks.push((progress.current, progress.total));
        }),
    )
    .unwrap();

    assert_eq!(ticks.last().unwrap().0, 3);
}

#[test]
fn bad_stamps_are_rejected() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");

    let mut doc = Document::open(&input).unwrap();
    let mut opts = text_opts("hi");
    opts.opacity = 2.0;
    let error = pdf_core::stamp(&mut doc, &range("all"), &opts).unwrap_err();
    assert!(
        matches!(error, PdfError::InvalidOpacity(_)),
        "got {error:?}"
    );

    let mut doc = Document::open(&input).unwrap();
    let error = pdf_core::stamp(&mut doc, &range("9"), &text_opts("hi")).unwrap_err();
    assert!(
        matches!(error, PdfError::PageOutOfRange { .. }),
        "got {error:?}"
    );
}
