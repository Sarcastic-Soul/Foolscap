//! Attachments, layers, page labels and article threads. Each fixture is
//! written to disk and read back, so every test exercises the file the user
//! would see.

mod common;

use common::{build_text_page, Workspace};
use lopdf::{dictionary, Dictionary, Object, StringFormat};
use pdf_core::{
    add_attachment, extract_attachment, list_articles, list_attachments, list_layers, page_labels,
    set_layer_visibility, set_page_labels, Document, LabelRange, LabelStyle, PdfError,
};

fn save(mut doc: Document, workspace: &Workspace, name: &str) -> std::path::PathBuf {
    let path = workspace.join(name);
    doc.save(&path).expect("could not save the document");
    path
}

// ---------------------------------------------------------------- attachments

#[test]
fn an_attachment_round_trips_through_a_saved_file() {
    let workspace = Workspace::new();
    let source =
        Document::open(workspace.write("base.pdf", build_text_page("hello", 24.0))).unwrap();
    let mut doc = source;

    let payload = b"col1,col2\n1,2\n".to_vec();
    add_attachment(&mut doc, "data.csv", &payload).unwrap();
    let path = save(doc, &workspace, "with-attachment.pdf");

    let reopened = Document::open(&path).unwrap();
    let listed = list_attachments(&reopened).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "data.csv");
    assert_eq!(listed[0].size, payload.len());
    assert_eq!(extract_attachment(&reopened, "data.csv").unwrap(), payload);
}

#[test]
fn adding_the_same_name_replaces_the_attachment() {
    let workspace = Workspace::new();
    let mut doc = Document::open(workspace.write("base.pdf", build_text_page("x", 24.0))).unwrap();

    add_attachment(&mut doc, "a.txt", b"first").unwrap();
    add_attachment(&mut doc, "b.txt", b"other").unwrap();
    add_attachment(&mut doc, "a.txt", b"second").unwrap();
    let path = save(doc, &workspace, "replaced.pdf");

    let reopened = Document::open(&path).unwrap();
    let names: Vec<String> = list_attachments(&reopened)
        .unwrap()
        .into_iter()
        .map(|attachment| attachment.name)
        .collect();
    assert_eq!(names, vec!["a.txt".to_string(), "b.txt".to_string()]);
    assert_eq!(extract_attachment(&reopened, "a.txt").unwrap(), b"second");
}

#[test]
fn a_missing_attachment_is_a_named_error() {
    let workspace = Workspace::new();
    let doc = Document::open(workspace.write("base.pdf", build_text_page("x", 24.0))).unwrap();
    assert!(list_attachments(&doc).unwrap().is_empty());
    assert!(matches!(
        extract_attachment(&doc, "nothing.txt"),
        Err(PdfError::NoSuchAttachment(name)) if name == "nothing.txt"
    ));
}

#[test]
fn an_attachment_needs_a_name() {
    let workspace = Workspace::new();
    let mut doc = Document::open(workspace.write("base.pdf", build_text_page("x", 24.0))).unwrap();
    assert!(matches!(
        add_attachment(&mut doc, "  ", b"data"),
        Err(PdfError::InvalidAttachment(_))
    ));
}

// ------------------------------------------------------------------- layers

/// A one-page document with two optional content groups, both shown.
fn layered(workspace: &Workspace, name: &str) -> std::path::PathBuf {
    let mut raw = build_text_page("layers", 24.0);
    let group = |label: &str| {
        dictionary! {
            "Type" => "OCG",
            "Name" => Object::String(label.as_bytes().to_vec(), StringFormat::Literal),
        }
    };
    let ocg_a = raw.add_object(group("Notes"));
    let ocg_b = raw.add_object(group("Watermark"));
    let properties = dictionary! {
        "OCGs" => vec![Object::Reference(ocg_a), Object::Reference(ocg_b)],
        "D" => dictionary! { "OFF" => Vec::<Object>::new() },
    };
    raw.catalog_mut()
        .unwrap()
        .set("OCProperties", Object::Dictionary(properties));
    workspace.write(name, raw)
}

#[test]
fn layers_are_listed_visible_by_default() {
    let workspace = Workspace::new();
    let doc = Document::open(layered(&workspace, "layers.pdf")).unwrap();
    let layers = list_layers(&doc).unwrap();
    let names: Vec<(&str, bool)> = layers
        .iter()
        .map(|layer| (layer.name.as_str(), layer.visible))
        .collect();
    assert_eq!(names, vec![("Notes", true), ("Watermark", true)]);
}

#[test]
fn hiding_a_layer_survives_a_save_and_showing_it_again_restores_it() {
    let workspace = Workspace::new();
    let mut doc = Document::open(layered(&workspace, "layers.pdf")).unwrap();

    set_layer_visibility(&mut doc, "Watermark", false).unwrap();
    let path = save(doc, &workspace, "hidden.pdf");
    let mut reopened = Document::open(&path).unwrap();

    let hidden: Vec<(String, bool)> = list_layers(&reopened)
        .unwrap()
        .into_iter()
        .map(|layer| (layer.name, layer.visible))
        .collect();
    assert_eq!(
        hidden,
        vec![("Notes".into(), true), ("Watermark".into(), false)]
    );

    set_layer_visibility(&mut reopened, "Watermark", true).unwrap();
    let shown = list_layers(&reopened).unwrap();
    assert!(shown.iter().all(|layer| layer.visible));
}

#[test]
fn an_unknown_layer_or_a_document_without_layers_is_an_error() {
    let workspace = Workspace::new();
    let mut with_layers = Document::open(layered(&workspace, "layers.pdf")).unwrap();
    assert!(matches!(
        set_layer_visibility(&mut with_layers, "Nope", false),
        Err(PdfError::NoSuchLayer(_))
    ));

    let mut plain =
        Document::open(workspace.write("plain.pdf", build_text_page("x", 24.0))).unwrap();
    assert!(list_layers(&plain).unwrap().is_empty());
    assert!(matches!(
        set_layer_visibility(&mut plain, "Notes", false),
        Err(PdfError::NoLayers)
    ));
}

// -------------------------------------------------------------- page labels

#[test]
fn front_matter_in_roman_then_arabic_numbers() {
    let workspace = Workspace::new();
    let mut doc = Document::open(workspace.write(
        "four.pdf",
        common::build(4, "p", (612, 792), common::Attributes::OnPage),
    ))
    .unwrap();

    set_page_labels(
        &mut doc,
        &[
            LabelRange {
                start_page: 1,
                style: LabelStyle::LowerRoman,
                prefix: String::new(),
                first: 1,
            },
            LabelRange {
                start_page: 3,
                style: LabelStyle::Decimal,
                prefix: "A-".into(),
                first: 1,
            },
        ],
    )
    .unwrap();
    let path = save(doc, &workspace, "labelled.pdf");

    let reopened = Document::open(&path).unwrap();
    assert_eq!(
        page_labels(&reopened).unwrap(),
        vec!["i", "ii", "A-1", "A-2"]
    );
}

#[test]
fn an_empty_range_list_clears_the_labels() {
    let workspace = Workspace::new();
    let mut doc = Document::open(workspace.write("two.pdf", build_text_page("x", 24.0))).unwrap();
    set_page_labels(
        &mut doc,
        &[LabelRange {
            start_page: 1,
            style: LabelStyle::UpperAlpha,
            prefix: String::new(),
            first: 1,
        }],
    )
    .unwrap();
    assert_eq!(page_labels(&doc).unwrap(), vec!["A"]);

    set_page_labels(&mut doc, &[]).unwrap();
    assert_eq!(page_labels(&doc).unwrap(), vec!["1"]);
}

#[test]
fn bad_label_ranges_are_refused() {
    let workspace = Workspace::new();
    let mut doc = Document::open(workspace.write("one.pdf", build_text_page("x", 24.0))).unwrap();
    let range = |start| LabelRange {
        start_page: start,
        style: LabelStyle::Decimal,
        prefix: String::new(),
        first: 1,
    };
    assert!(set_page_labels(&mut doc, &[range(0)]).is_err());
    assert!(set_page_labels(&mut doc, &[range(3), range(2)]).is_err());
}

// ------------------------------------------------------------------ articles

#[test]
fn an_article_thread_lists_its_beads_and_first_page() {
    let workspace = Workspace::new();
    let mut raw = build_text_page("article", 24.0);
    let page_id = *raw.get_pages().values().next().unwrap();

    let bead_a = raw.new_object_id();
    let bead_b = raw.new_object_id();
    let rect = Object::Array(vec![0.into(), 0.into(), 100.into(), 100.into()]);
    raw.objects.insert(
        bead_a,
        Object::Dictionary(dictionary! {
            "Type" => "Bead",
            "P" => page_id,
            "R" => rect.clone(),
            "N" => bead_b,
            "V" => bead_b,
        }),
    );
    raw.objects.insert(
        bead_b,
        Object::Dictionary(dictionary! {
            "Type" => "Bead",
            "P" => page_id,
            "R" => rect,
            "N" => bead_a,
            "V" => bead_a,
        }),
    );

    let info: Dictionary = dictionary! {
        "Title" => Object::String(b"Main story".to_vec(), StringFormat::Literal),
    };
    let thread = dictionary! {
        "I" => info,
        "F" => bead_a,
    };
    raw.catalog_mut()
        .unwrap()
        .set("Threads", Object::Array(vec![Object::Dictionary(thread)]));

    let doc = Document::open(workspace.write("threads.pdf", raw)).unwrap();
    let articles = list_articles(&doc).unwrap();
    assert_eq!(articles.len(), 1);
    assert_eq!(articles[0].title, "Main story");
    assert_eq!(articles[0].beads, 2);
    assert_eq!(articles[0].first_page, Some(1));
}

#[test]
fn a_document_without_threads_has_no_articles() {
    let workspace = Workspace::new();
    let doc = Document::open(workspace.write("plain.pdf", build_text_page("x", 24.0))).unwrap();
    assert!(list_articles(&doc).unwrap().is_empty());
}
