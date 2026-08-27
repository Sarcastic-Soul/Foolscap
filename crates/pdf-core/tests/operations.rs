//! End-to-end tests for the stage 1 operations.
//!
//! Every test writes real PDFs to a temporary directory and reads them back
//! with a fresh parse, so what is asserted is what would land on disk.

mod common;

use common::{Attributes, Workspace, A4, LETTER};
use lopdf::dictionary;
use pdf_core::{Document, Metadata, MetadataEdit, OptimizeLevel, PageRange, PdfError, SplitSpec};

fn range(spec: &str) -> PageRange {
    PageRange::parse(spec).expect("test range should parse")
}

// ---------------------------------------------------------------- opening

#[test]
fn opening_a_missing_file_says_so() {
    let workspace = Workspace::new();
    let error = Document::open(workspace.join("absent.pdf")).unwrap_err();
    assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
}

#[test]
fn encrypted_documents_are_refused_by_name() {
    let workspace = Workspace::new();

    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    let encrypt_id = doc.add_object(dictionary! {
        "Filter" => "Standard",
        "V" => 1,
        "R" => 2,
        "O" => lopdf::Object::string_literal(vec![0u8; 32]),
        "U" => lopdf::Object::string_literal(vec![0u8; 32]),
        "P" => -1,
    });
    doc.trailer
        .set("Encrypt", lopdf::Object::Reference(encrypt_id));
    let path = workspace.write("encrypted.pdf", doc);

    let error = Document::open(&path).unwrap_err();
    assert!(matches!(error, PdfError::Encrypted(_)), "got {error:?}");
}

#[test]
fn page_count_matches_what_was_written() {
    let workspace = Workspace::new();
    let path = workspace.document("five.pdf", 5, "Page");
    assert_eq!(Document::open(&path).unwrap().page_count(), 5);
}

// ------------------------------------------------------------------ merge

#[test]
fn merging_sums_the_page_counts() {
    let workspace = Workspace::new();
    let a = workspace.document("a.pdf", 3, "A");
    let b = workspace.document("b.pdf", 2, "B");
    let out = workspace.join("merged.pdf");

    pdf_core::merge(&[a, b], &out).unwrap();

    assert_eq!(Document::open(&out).unwrap().page_count(), 5);
}

#[test]
fn merging_keeps_pages_in_input_order() {
    let workspace = Workspace::new();
    let a = workspace.document("a.pdf", 2, "A");
    let b = workspace.document("b.pdf", 2, "B");
    let out = workspace.join("merged.pdf");

    pdf_core::merge(&[a, b], &out).unwrap();

    assert_eq!(common::page_labels(&out), ["A 1", "A 2", "B 1", "B 2"]);
}

#[test]
fn merging_preserves_each_input_page_size() {
    // The regression this guards: both inputs keep their page geometry on the
    // /Pages node, so a merge that repoints pages at a shared parent without
    // materialising inherited attributes silently resizes one of them.
    let workspace = Workspace::new();
    let a = workspace.write("a4.pdf", common::build(2, "A", A4, Attributes::Inherited));
    let b = workspace.write(
        "letter.pdf",
        common::build(2, "B", LETTER, Attributes::Inherited),
    );
    let out = workspace.join("merged.pdf");

    pdf_core::merge(&[a, b], &out).unwrap();

    assert_eq!(
        common::page_media_boxes(&out),
        vec![
            vec![0, 0, A4.0, A4.1],
            vec![0, 0, A4.0, A4.1],
            vec![0, 0, LETTER.0, LETTER.1],
            vec![0, 0, LETTER.0, LETTER.1],
        ]
    );
}

#[test]
fn merging_a_single_document_is_allowed() {
    let workspace = Workspace::new();
    let a = workspace.document("a.pdf", 3, "A");
    let out = workspace.join("merged.pdf");

    pdf_core::merge(&[a], &out).unwrap();

    assert_eq!(common::page_labels(&out), ["A 1", "A 2", "A 3"]);
}

#[test]
fn merging_nothing_is_an_error() {
    let workspace = Workspace::new();
    let error = pdf_core::merge(&[], &workspace.join("merged.pdf")).unwrap_err();
    assert!(matches!(error, PdfError::EmptySelection), "got {error:?}");
}

// ------------------------------------------------------------------ split

#[test]
fn extracting_selects_the_named_pages() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 5, "Page");

    let written = pdf_core::split(
        &input,
        &SplitSpec::Extract(range("1-2,5")),
        workspace.path(),
    )
    .unwrap();

    assert_eq!(written.len(), 1);
    assert_eq!(
        common::page_labels(&written[0]),
        ["Page 1", "Page 2", "Page 5"]
    );
}

#[test]
fn extracting_honours_the_order_that_was_asked_for() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 4, "Page");

    let written =
        pdf_core::split(&input, &SplitSpec::Extract(range("3,1")), workspace.path()).unwrap();

    assert_eq!(common::page_labels(&written[0]), ["Page 3", "Page 1"]);
}

#[test]
fn repeating_a_page_produces_two_independent_pages() {
    // A page object referenced twice in the page tree is not a valid document,
    // so the second occurrence has to be a copy with its own object id.
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");

    let written =
        pdf_core::split(&input, &SplitSpec::Extract(range("2,2")), workspace.path()).unwrap();

    assert_eq!(common::page_labels(&written[0]), ["Page 2", "Page 2"]);

    let ids = common::page_ids(&written[0]);
    assert_eq!(ids.len(), 2);
    assert_ne!(
        ids[0], ids[1],
        "the repeated page must be a distinct object"
    );
}

#[test]
fn extracting_out_of_range_pages_is_an_error() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");

    let error =
        pdf_core::split(&input, &SplitSpec::Extract(range("9")), workspace.path()).unwrap_err();

    assert!(
        matches!(
            error,
            PdfError::PageOutOfRange {
                requested: 9,
                total: 3
            }
        ),
        "got {error:?}"
    );
}

#[test]
fn bursting_writes_one_file_per_page() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");

    let written = pdf_core::split(&input, &SplitSpec::Every(1), workspace.path()).unwrap();

    assert_eq!(written.len(), 3);
    for (index, path) in written.iter().enumerate() {
        assert_eq!(common::page_labels(path), [format!("Page {}", index + 1)]);
    }
}

#[test]
fn bursting_leaves_a_short_final_chunk() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 5, "Page");

    let written = pdf_core::split(&input, &SplitSpec::Every(2), workspace.path()).unwrap();

    assert_eq!(written.len(), 3);
    assert_eq!(common::page_labels(&written[2]), ["Page 5"]);
}

#[test]
fn bursting_then_merging_reproduces_the_original() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 6, "Page");
    let before = common::page_labels(&input);

    let pieces = pdf_core::split(&input, &SplitSpec::Every(1), workspace.path()).unwrap();
    let rejoined = workspace.join("rejoined.pdf");
    pdf_core::merge(&pieces, &rejoined).unwrap();

    assert_eq!(common::page_labels(&rejoined), before);
}

#[test]
fn bursting_by_zero_is_rejected() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");

    let error = pdf_core::split(&input, &SplitSpec::Every(0), workspace.path()).unwrap_err();

    assert!(
        matches!(error, PdfError::InvalidPageRange { .. }),
        "got {error:?}"
    );
}

#[test]
fn splitting_preserves_inherited_page_size() {
    let workspace = Workspace::new();
    let input = workspace.write(
        "letter.pdf",
        common::build(3, "Page", LETTER, Attributes::Inherited),
    );

    let written =
        pdf_core::split(&input, &SplitSpec::Extract(range("2")), workspace.path()).unwrap();

    assert_eq!(
        common::page_media_boxes(&written[0]),
        vec![vec![0, 0, LETTER.0, LETTER.1]]
    );
}

// ----------------------------------------------------------------- rotate

fn rotation_of(path: &std::path::Path) -> Vec<i64> {
    let doc = lopdf::Document::load(path).unwrap();
    doc.get_pages()
        .into_values()
        .map(|page_id| {
            doc.get_dictionary(page_id)
                .ok()
                .and_then(|dict| dict.get(b"Rotate").ok())
                .and_then(|object| object.as_i64().ok())
                .unwrap_or(0)
        })
        .collect()
}

#[test]
fn rotating_touches_only_the_selected_pages() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 4, "Page");
    let out = workspace.join("rotated.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::rotate(&mut doc, &range("2-3"), 90).unwrap();
    doc.save(&out).unwrap();

    assert_eq!(rotation_of(&out), [0, 90, 90, 0]);
}

#[test]
fn rotation_accumulates_and_wraps() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let once = workspace.join("once.pdf");
    let twice = workspace.join("twice.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::rotate(&mut doc, &range("all"), 270).unwrap();
    doc.save(&once).unwrap();
    assert_eq!(rotation_of(&once), [270]);

    let mut doc = Document::open(&once).unwrap();
    pdf_core::rotate(&mut doc, &range("all"), 180).unwrap();
    doc.save(&twice).unwrap();
    // 270 + 180 = 450, which is 90 once folded into a single turn.
    assert_eq!(rotation_of(&twice), [90]);
}

#[test]
fn rotating_back_to_zero_removes_the_entry() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::rotate(&mut doc, &range("all"), 360).unwrap();
    doc.save(&out).unwrap();

    let reloaded = lopdf::Document::load(&out).unwrap();
    let page_id = reloaded.get_pages().into_values().next().unwrap();
    assert!(reloaded
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Rotate")
        .is_err());
}

#[test]
fn selecting_a_page_twice_rotates_it_once() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 2, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::rotate(&mut doc, &range("1,1"), 90).unwrap();
    doc.save(&out).unwrap();

    assert_eq!(rotation_of(&out), [90, 0]);
}

#[test]
fn rotating_by_a_non_multiple_of_ninety_is_rejected() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");

    let mut doc = Document::open(&input).unwrap();
    let error = pdf_core::rotate(&mut doc, &range("all"), 45).unwrap_err();

    assert!(
        matches!(error, PdfError::InvalidRotation(45)),
        "got {error:?}"
    );
}

// --------------------------------------------------------------- metadata

#[test]
fn metadata_round_trips_including_non_ascii() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let out = workspace.join("out.pdf");

    let written = Metadata {
        title: Some("Rapport trimestriel — Q3".to_string()),
        author: Some("Anish".to_string()),
        subject: Some("日本語".to_string()),
        keywords: Some("pdf, test".to_string()),
        ..Default::default()
    };

    let mut doc = Document::open(&input).unwrap();
    doc.set_metadata(&written).unwrap();
    doc.save(&out).unwrap();

    let read = Document::open(&out).unwrap().metadata().unwrap();
    assert_eq!(read.title, written.title);
    assert_eq!(read.author, written.author);
    assert_eq!(read.subject, written.subject);
    assert_eq!(read.keywords, written.keywords);
}

#[test]
fn a_document_without_an_info_dictionary_reads_as_empty() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");

    assert!(Document::open(&input)
        .unwrap()
        .metadata()
        .unwrap()
        .is_empty());
}

#[test]
fn editing_leaves_untouched_fields_alone() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let first = workspace.join("first.pdf");
    let second = workspace.join("second.pdf");

    let mut doc = Document::open(&input).unwrap();
    doc.set_metadata(&Metadata {
        title: Some("Original".into()),
        author: Some("Anish".into()),
        ..Default::default()
    })
    .unwrap();
    doc.save(&first).unwrap();

    let edit = MetadataEdit {
        title: Some(Some("Renamed".into())),
        ..Default::default()
    };

    let mut doc = Document::open(&first).unwrap();
    let updated = edit.apply(&doc.metadata().unwrap());
    doc.set_metadata(&updated).unwrap();
    doc.save(&second).unwrap();

    let read = Document::open(&second).unwrap().metadata().unwrap();
    assert_eq!(read.title.as_deref(), Some("Renamed"));
    assert_eq!(read.author.as_deref(), Some("Anish"));
}

#[test]
fn clearing_a_field_removes_it() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let first = workspace.join("first.pdf");
    let second = workspace.join("second.pdf");

    let mut doc = Document::open(&input).unwrap();
    doc.set_metadata(&Metadata {
        title: Some("Original".into()),
        ..Default::default()
    })
    .unwrap();
    doc.save(&first).unwrap();

    let edit = MetadataEdit {
        title: Some(None),
        ..Default::default()
    };

    let mut doc = Document::open(&first).unwrap();
    let updated = edit.apply(&doc.metadata().unwrap());
    doc.set_metadata(&updated).unwrap();
    doc.save(&second).unwrap();

    assert_eq!(
        Document::open(&second).unwrap().metadata().unwrap().title,
        None
    );
}

#[test]
fn metadata_survives_a_merge() {
    let workspace = Workspace::new();
    let a = workspace.document("a.pdf", 1, "A");
    let titled = workspace.join("titled.pdf");
    let out = workspace.join("merged.pdf");

    let mut doc = Document::open(&a).unwrap();
    doc.set_metadata(&Metadata {
        title: Some("First input".into()),
        ..Default::default()
    })
    .unwrap();
    doc.save(&titled).unwrap();

    let b = workspace.document("b.pdf", 1, "B");
    pdf_core::merge(&[titled, b], &out).unwrap();

    assert_eq!(
        Document::open(&out)
            .unwrap()
            .metadata()
            .unwrap()
            .title
            .as_deref(),
        Some("First input")
    );
}

// --------------------------------------------------------------- optimize

#[test]
fn optimizing_keeps_every_page() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 4, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    pdf_core::optimize(&mut doc, OptimizeLevel::Safe).unwrap();
    doc.save(&out).unwrap();

    assert_eq!(
        common::page_labels(&out),
        ["Page 1", "Page 2", "Page 3", "Page 4"]
    );
}

#[test]
fn optimizing_drops_unreachable_objects() {
    let workspace = Workspace::new();

    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    // An object nothing refers to: exactly what a prune pass exists for.
    doc.add_object(lopdf::Object::string_literal("orphaned".repeat(64)));
    let input = workspace.write("in.pdf", doc);

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::optimize(&mut doc, OptimizeLevel::Safe).unwrap();

    assert!(
        report.objects_removed >= 1,
        "expected the orphan to be pruned, removed {}",
        report.objects_removed
    );
}

#[test]
fn aggressive_optimization_deduplicates_identical_streams() {
    let workspace = Workspace::new();
    // Every page in a generated document draws different text, so build one
    // where two pages share byte-identical content instead.
    let input = workspace.document("in.pdf", 1, "Page");
    let doubled = workspace.join("doubled.pdf");

    // Two copies of the same page give two byte-identical content streams.
    pdf_core::merge(&[input.clone(), input], &doubled).unwrap();

    let mut doc = Document::open(&doubled).unwrap();
    let report = pdf_core::optimize(&mut doc, OptimizeLevel::Aggressive).unwrap();

    assert!(
        report.streams_deduplicated >= 1,
        "expected the duplicated content stream to be collapsed"
    );

    let out = workspace.join("out.pdf");
    doc.save(&out).unwrap();
    assert_eq!(common::page_labels(&out), ["Page 1", "Page 1"]);
}

#[test]
fn the_report_describes_the_bytes_that_will_be_written() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::optimize(&mut doc, OptimizeLevel::Safe).unwrap();
    doc.save(&out).unwrap();

    let actual = std::fs::metadata(&out).unwrap().len();
    assert_eq!(report.bytes_after, actual);
}

#[test]
fn the_plan_matches_what_split_actually_writes() {
    // The CLI asks before overwriting, and it can only do that if the names it
    // is given up front are the names that end up on disk.
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 5, "Page");

    for spec in [
        SplitSpec::Every(1),
        SplitSpec::Every(2),
        SplitSpec::Extract(range("1-3")),
    ] {
        let planned = pdf_core::split_plan(&input, &spec, workspace.path()).unwrap();
        let written = pdf_core::split(&input, &spec, workspace.path()).unwrap();
        assert_eq!(planned, written, "plan disagreed with reality for {spec:?}");
    }
}
