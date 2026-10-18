//! End-to-end tests for the stage 1 operations.
//!
//! Every test writes real PDFs to a temporary directory and reads them back
//! with a fresh parse, so what is asserted is what would land on disk.

mod common;

use common::{add_annotation, Attributes, Normal, Workspace, A4, LETTER};
use lopdf::content::Operation;
use lopdf::{dictionary, Object, Stream};
use pdf_core::{
    Document, FlattenOptions, Metadata, MetadataEdit, OptimizeLevel, PageRange, PdfError, SplitSpec,
};

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
        let planned = pdf_core::split_plan(&input, &spec, workspace.path(), None).unwrap();
        let written = pdf_core::split(&input, &spec, workspace.path()).unwrap();
        assert_eq!(planned, written, "plan disagreed with reality for {spec:?}");
    }
}

// ---------------------------------------------------------------- arrange

#[test]
fn deleting_removes_only_the_named_pages() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 5, "Page");
    let out = workspace.join("out.pdf");

    let doc = Document::open(&input).unwrap();
    let mut trimmed = pdf_core::delete(doc, &range("2,4")).unwrap();
    trimmed.save(&out).unwrap();

    assert_eq!(common::page_labels(&out), ["Page 1", "Page 3", "Page 5"]);
}

#[test]
fn deleting_every_page_is_refused() {
    // A PDF with no pages is not something anyone can open.
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");

    let doc = Document::open(&input).unwrap();
    let error = pdf_core::delete(doc, &range("all")).unwrap_err();

    assert!(matches!(error, PdfError::EmptySelection), "got {error:?}");
}

#[test]
fn arranging_reorders_and_can_duplicate() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");
    let out = workspace.join("out.pdf");

    let doc = Document::open(&input).unwrap();
    let mut arranged = pdf_core::arrange(doc, &[2, 0, 0]).unwrap();
    arranged.save(&out).unwrap();

    assert_eq!(common::page_labels(&out), ["Page 3", "Page 1", "Page 1"]);
}

#[test]
fn moving_a_page_keeps_every_other_page() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 5, "Page");
    let out = workspace.join("out.pdf");

    let doc = Document::open(&input).unwrap();
    let mut moved = pdf_core::move_pages(doc, &range("1"), 3).unwrap();
    moved.save(&out).unwrap();

    assert_eq!(
        common::page_labels(&out),
        ["Page 2", "Page 3", "Page 1", "Page 4", "Page 5"]
    );
}

#[test]
fn moving_past_the_end_is_an_error() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");

    let doc = Document::open(&input).unwrap();
    let error = pdf_core::move_pages(doc, &range("1"), 9).unwrap_err();

    assert!(
        matches!(error, PdfError::PageOutOfRange { .. }),
        "got {error:?}"
    );
}

#[test]
fn arranging_preserves_inherited_page_geometry() {
    let workspace = Workspace::new();
    let input = workspace.write(
        "letter.pdf",
        common::build(3, "Page", LETTER, Attributes::Inherited),
    );
    let out = workspace.join("out.pdf");

    let doc = Document::open(&input).unwrap();
    let mut arranged = pdf_core::arrange(doc, &[2, 1, 0]).unwrap();
    arranged.save(&out).unwrap();

    for media_box in common::page_media_boxes(&out) {
        assert_eq!(media_box, vec![0, 0, LETTER.0, LETTER.1]);
    }
}

// ------------------------------------------------------------------ security

/// A document carrying everything `strip` should remove: standard Info keys,
/// a producer custom key, and an XMP packet referenced from the Catalog.
fn identifiable(workspace: &Workspace, name: &str) -> std::path::PathBuf {
    let mut doc = common::build(2, "Page", A4, Attributes::OnPage);
    let info = doc.add_object(dictionary! {
        "Title" => Object::string_literal("Secret plan"),
        "Author" => Object::string_literal("Anon"),
        "CreationDate" => Object::string_literal("D:20260101000000Z"),
        "MyCustomKey" => Object::string_literal("traces"),
    });
    doc.trailer.set("Info", Object::Reference(info));

    let xmp = doc.add_object(lopdf::Stream::new(
        dictionary! {
            "Type" => "Metadata",
            "Subtype" => "XML",
        },
        b"<x:xmpmeta>creator tool</x:xmpmeta>".to_vec(),
    ));
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(|object| object.as_reference())
        .expect("a catalog reference");
    doc.get_dictionary_mut(root)
        .expect("a catalog")
        .set("Metadata", Object::Reference(xmp));

    workspace.write(name, doc)
}

/// The trailer ID as raw bytes, read without interpreting the document.
fn trailer_id(path: &std::path::Path) -> Vec<u8> {
    let doc = lopdf::Document::load(path).expect("could not reload document");
    format!("{:?}", doc.trailer.get(b"ID")).into_bytes()
}

#[test]
fn stripping_removes_all_identifying_metadata() {
    let workspace = Workspace::new();
    let input = identifiable(&workspace, "in.pdf");
    let out = workspace.join("out.pdf");
    let before = trailer_id(&input);

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::strip(&mut doc).unwrap();
    assert!(report.info_keys_removed >= 4, "got {report:?}");
    assert_eq!(report.xmp_removed, 1, "got {report:?}");
    assert!(report.id_rotated);
    doc.save(&out).unwrap();

    let stripped = Document::open(&out).unwrap();
    assert_eq!(stripped.metadata().unwrap().title, None);

    // The custom key must not survive in the raw bytes either.
    let bytes = std::fs::read(&out).unwrap();
    assert!(!bytes.windows(11).any(|window| window == b"MyCustomKey"));
    assert!(!bytes.windows(11).any(|window| window == b"Secret plan"));

    // No Catalog Metadata reference and no XMP stream left behind.
    let raw = lopdf::Document::load(&out).unwrap();
    let root = raw
        .trailer
        .get(b"Root")
        .and_then(|o| o.as_reference())
        .unwrap();
    assert!(raw.get_dictionary(root).unwrap().get(b"Metadata").is_err());
    assert!(
        !raw.objects.values().any(|object| matches!(object,
            Object::Stream(stream) if stream.dict.get(b"Subtype")
                .and_then(Object::as_name).is_ok_and(|name| name == b"XML"))),
        "an XMP stream survived the strip"
    );

    assert_ne!(trailer_id(&out), before, "the trailer ID was not rotated");
    assert_eq!(common::page_labels(&out), ["Page 1", "Page 2"]);
}

#[test]
fn stripping_a_plain_document_keeps_it_readable() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let out = workspace.join("out.pdf");

    let mut doc = Document::open(&input).unwrap();
    doc.save(&out).unwrap();
    let plain_id = trailer_id(&out);

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::strip(&mut doc).unwrap();
    assert_eq!(report.info_keys_removed, 0);
    assert_eq!(report.xmp_removed, 0);
    doc.save(&out).unwrap();

    // Even with nothing to remove, the ID rotates so the export cannot be
    // correlated with the source by identifier.
    assert_ne!(trailer_id(&out), plain_id);
    assert_eq!(common::page_labels(&out), ["Page 1"]);
}

// ------------------------------------------------------------------ repair

/// Create a PDF whose content stream declares more data than it holds.
fn truncated_stream_pdf(workspace: &Workspace) -> std::path::PathBuf {
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);

    let stream_id = doc.add_object(Stream::new(dictionary! {}, b"0 g 0 0 10 10 re f".to_vec()));
    let page_id = doc.get_pages().values().next().copied().unwrap();
    doc.get_dictionary_mut(page_id)
        .unwrap()
        .set("Contents", Object::Reference(stream_id));

    let path = workspace.write("truncated.pdf", doc);

    // lopdf always writes the true length, so make the file lie afterwards.
    let bytes = std::fs::read(&path).unwrap();
    let needle = b"/Length 18";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the stream's length should be written directly");
    let mut lying = bytes[..at].to_vec();
    lying.extend_from_slice(b"/Length 90");
    lying.extend_from_slice(&bytes[at + needle.len()..]);
    std::fs::write(&path, lying).unwrap();
    path
}

/// Create a PDF whose `startxref` points past the end of the file, so the
/// cross-reference data cannot be found.
fn bad_xref_pdf(workspace: &Workspace) -> std::path::PathBuf {
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    let path = workspace.join("bad_xref.pdf");
    doc.save(&path).unwrap();

    let bytes = std::fs::read(&path).unwrap();
    let marker = b"startxref\n";
    let at = bytes
        .windows(marker.len())
        .rposition(|w| w == marker)
        .expect("a saved file ends with startxref")
        + marker.len();
    let digits = bytes[at..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();

    let mut damaged = bytes[..at].to_vec();
    damaged.extend_from_slice(b"99999999");
    damaged.extend_from_slice(&bytes[at + digits..]);
    std::fs::write(&path, damaged).unwrap();
    path
}

/// Create a PDF with bookmarks (outlines).
fn bookmark_pdf(workspace: &Workspace) -> std::path::PathBuf {
    let mut doc = common::build(4, "Page", A4, Attributes::OnPage);

    // Page IDs in order.
    let page_ids: Vec<lopdf::ObjectId> = doc.get_pages().into_values().collect();

    // Create outline entries.
    let outline1 = doc.add_object(dictionary! {
        "Type" => "Outlines",
        "Title" => "Chapter 1",
        "Dest" => Object::Array(vec![
            Object::Reference(page_ids[0]),
            Object::Name(b"XYZ".to_vec()),
            Object::Integer(0),
            Object::Integer(842),
            Object::Null,
        ]),
    });
    let outline2 = doc.add_object(dictionary! {
        "Type" => "Outlines",
        "Title" => "Chapter 2",
        "Dest" => Object::Array(vec![
            Object::Reference(page_ids[2]),
            Object::Name(b"XYZ".to_vec()),
            Object::Integer(0),
            Object::Integer(842),
            Object::Null,
        ]),
    });

    // Link them as siblings.
    doc.get_dictionary_mut(outline1)
        .unwrap()
        .set("Next", Object::Reference(outline2));
    doc.get_dictionary_mut(outline2)
        .unwrap()
        .set("Prev", Object::Reference(outline1));

    // Create the Outlines root.
    let outlines_root = doc.add_object(dictionary! {
        "Type" => "Outlines",
        "First" => Object::Reference(outline1),
        "Last" => Object::Reference(outline2),
        "Count" => 2,
    });

    // Attach to catalog.
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(|o| o.as_reference())
        .unwrap();
    doc.get_dictionary_mut(root)
        .unwrap()
        .set("Outlines", Object::Reference(outlines_root));

    workspace.write("bookmarks.pdf", doc)
}

#[test]
fn repair_clean_pdf_is_noop() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");
    let out = workspace.join("repaired.pdf");

    let report = pdf_core::repair(&input, &out).unwrap();

    assert!(report.is_clean(), "{report:?}");
    assert_eq!(Document::open(&out).unwrap().page_count(), 3);
}

#[test]
fn repair_fixes_truncated_stream() {
    let workspace = Workspace::new();
    let input = truncated_stream_pdf(&workspace);
    let out = workspace.join("repaired.pdf");

    let report = pdf_core::repair(&input, &out).unwrap();

    assert_eq!(report.fixed_streams, 1);
    let repaired = Document::open(&out).unwrap();
    assert_eq!(repaired.page_count(), 1);
    // The page keeps its whole content stream, and nothing past its end.
    let reloaded = lopdf::Document::load(&out).unwrap();
    let page = reloaded.get_pages().values().next().copied().unwrap();
    let content = reloaded.get_page_content(page);
    assert_eq!(content.trim_ascii(), b"0 g 0 0 10 10 re f");
}

#[test]
fn repair_recovers_from_bad_xref() {
    let workspace = Workspace::new();
    let input = bad_xref_pdf(&workspace);
    let out = workspace.join("repaired.pdf");

    let report = pdf_core::repair(&input, &out).unwrap();

    assert!(report.recovered_objects > 0, "{report:?}");
    assert!(!report.warnings.is_empty());
    assert_eq!(common::page_labels(&out), ["Page 1"]);
}

#[test]
fn repair_loaded_leaves_a_document_usable() {
    let workspace = Workspace::new();
    let input = truncated_stream_pdf(&workspace);

    // The file lies about a length, which lopdf takes at its word on load, so
    // there is nothing left for the in-memory pass to correct; it should leave
    // the document usable and say what little it did.
    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::repair_loaded(&mut doc);

    assert_eq!(report.recovered_objects, 0);
    let out = workspace.join("repaired.pdf");
    doc.save(&out).unwrap();
    assert_eq!(Document::open(&out).unwrap().page_count(), 1);
}

// ------------------------------------------------------------------ split enhanced

#[test]
fn split_by_bookmarks_matches_outline_boundaries() {
    let workspace = Workspace::new();
    let input = bookmark_pdf(&workspace);

    let written = pdf_core::split(&input, &SplitSpec::ByBookmarks, workspace.path()).unwrap();

    // Should produce 3 chunks: pages 0-1 (Chapter 1), pages 2-3 (Chapter 2),
    // and potentially a leading chunk if first bookmark doesn't start at page 0.
    // Our test has bookmarks at pages 0 and 2 (0-indexed), so:
    // - Chunk 0: pages 0-1 (Chapter 1 covers pages 0,1)
    // - Chunk 1: pages 2-3 (Chapter 2 covers pages 2,3)
    assert_eq!(written.len(), 2);

    // First chunk: pages 1,2 (1-indexed in labels)
    assert_eq!(common::page_labels(&written[0]), ["Page 1", "Page 2"]);
    // Second chunk: pages 3,4
    assert_eq!(common::page_labels(&written[1]), ["Page 3", "Page 4"]);
}

#[test]
fn split_by_pages_is_synonym_for_every() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 5, "Page");

    let by_pages = pdf_core::split(&input, &SplitSpec::ByPages(2), workspace.path()).unwrap();
    let every = pdf_core::split(&input, &SplitSpec::Every(2), workspace.path()).unwrap();

    assert_eq!(by_pages.len(), every.len());
    for (bp, ev) in by_pages.iter().zip(every.iter()) {
        assert_eq!(common::page_labels(bp), common::page_labels(ev));
    }
}

#[test]
fn split_by_size_greedy_packs_pages() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 6, "Page");

    // Each page is roughly the same size. With a small limit, we get many chunks.
    // With a large limit, we get fewer.
    let small = pdf_core::split(&input, &SplitSpec::BySize(1), workspace.path()).unwrap();
    let large = pdf_core::split(&input, &SplitSpec::BySize(100), workspace.path()).unwrap();

    // Small limit (1 MB) should produce more chunks than large (100 MB).
    // The exact count depends on page size, but small should have >= chunks.
    assert!(small.len() >= large.len());

    // Every page should appear exactly once across all chunks.
    let mut all_pages = Vec::new();
    for path in &small {
        all_pages.extend(common::page_labels(path));
    }
    all_pages.sort();
    assert_eq!(
        all_pages,
        ["Page 1", "Page 2", "Page 3", "Page 4", "Page 5", "Page 6"]
    );
}

#[test]
fn split_by_size_single_large_page_gets_own_chunk() {
    let workspace = Workspace::new();

    // Page 1 carries a 3 MB uncompressed image, well over the 1 MB limit;
    // page 2 is tiny.
    let mut doc = common::build(2, "Page", A4, Attributes::OnPage);
    let image_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1000,
            "Height" => 1000,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
        },
        vec![0u8; 3_000_000],
    ));
    let page_id = doc.get_pages().values().next().copied().unwrap();
    let draw = lopdf::content::Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    500.into(),
                    0.into(),
                    0.into(),
                    500.into(),
                    50.into(),
                    100.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Im1".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    let draw_id = doc.add_object(Stream::new(dictionary! {}, draw.encode().unwrap()));

    // Keep the page's own content (its label) and draw the image after it.
    let original = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .clone();
    let mut resources = match doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .clone()
    {
        Object::Reference(id) => doc.get_dictionary(id).unwrap().clone(),
        Object::Dictionary(dict) => dict,
        other => panic!("unexpected /Resources {other:?}"),
    };
    resources.set(
        "XObject",
        Object::Dictionary(dictionary! { "Im1" => image_id }),
    );
    let page = doc.get_dictionary_mut(page_id).unwrap();
    page.set("Resources", Object::Dictionary(resources));
    page.set(
        "Contents",
        Object::Array(vec![original, Object::Reference(draw_id)]),
    );

    let input = workspace.write("mixed.pdf", doc);
    let written = pdf_core::split(&input, &SplitSpec::BySize(1), workspace.path()).unwrap();

    assert_eq!(written.len(), 2);
    assert_eq!(common::page_labels(&written[0]), ["Page 1"]);
    assert_eq!(common::page_labels(&written[1]), ["Page 2"]);
}

#[test]
fn split_by_size_zero_is_rejected() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 2, "Page");

    let error = pdf_core::split(&input, &SplitSpec::BySize(0), workspace.path()).unwrap_err();
    assert!(
        matches!(error, PdfError::InvalidPageRange { .. }),
        "got {error:?}"
    );
}

#[test]
fn split_by_bookmarks_without_outlines_gives_one_piece() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 3, "Page");

    let written = pdf_core::split(&input, &SplitSpec::ByBookmarks, workspace.path()).unwrap();
    assert_eq!(written.len(), 1);
    assert_eq!(
        common::page_labels(&written[0]),
        ["Page 1", "Page 2", "Page 3"]
    );
}

#[test]
fn split_plan_matches_enhanced_specs() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 5, "Page");

    for spec in [SplitSpec::ByPages(2), SplitSpec::BySize(10)] {
        let planned = pdf_core::split_plan(&input, &spec, workspace.path(), None).unwrap();
        let written = pdf_core::split(&input, &spec, workspace.path()).unwrap();
        assert_eq!(planned, written, "plan disagreed with reality for {spec:?}");
    }
}

// --------------------------------------------------------------- flatten

/// Decode a page's content into operations, from a fresh parse of `path`.
fn page_operations(path: &std::path::Path, page_number: u32) -> Vec<Operation> {
    let doc = lopdf::Document::load(path).unwrap();
    let page_id = doc.get_pages()[&page_number];
    lopdf::content::Content::decode(&doc.get_page_content(page_id))
        .unwrap()
        .operations
}

/// The text drawn by the stream a page's `Do` operators point at, in order.
fn drawn_appearances(path: &std::path::Path, page_number: u32) -> Vec<String> {
    let doc = lopdf::Document::load(path).unwrap();
    let page_id = doc.get_pages()[&page_number];
    let resources = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Resources")
        .and_then(|r| doc.dereference(r))
        .map(|(_, r)| r.as_dict().unwrap().clone())
        .unwrap();
    let xobjects = resources
        .get(b"XObject")
        .and_then(|x| doc.dereference(x))
        .map(|(_, x)| x.as_dict().unwrap().clone())
        .unwrap_or_default();

    page_operations(path, page_number)
        .iter()
        .filter(|op| op.operator == "Do")
        .filter_map(|op| {
            let name = op.operands[0].as_name().ok()?;
            let id = xobjects.get(name).ok()?.as_reference().ok()?;
            let stream = doc.get_object(id).ok()?.as_stream().ok()?;
            let content = stream
                .decompressed_content()
                .unwrap_or_else(|_| stream.content.clone());
            let decoded = lopdf::content::Content::decode(&content).unwrap();
            Some(
                decoded
                    .operations
                    .iter()
                    .filter(|op| op.operator == "Tj")
                    .filter_map(|op| match &op.operands[0] {
                        Object::String(bytes, _) => {
                            Some(String::from_utf8_lossy(bytes).into_owned())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            )
        })
        .collect()
}

fn annot_count(path: &std::path::Path, page_number: u32) -> usize {
    let doc = lopdf::Document::load(path).unwrap();
    let page_id = doc.get_pages()[&page_number];
    match doc.get_dictionary(page_id).unwrap().get(b"Annots") {
        Ok(Object::Array(items)) => items.len(),
        _ => 0,
    }
}

fn has_acroform(path: &std::path::Path) -> bool {
    let doc = lopdf::Document::load(path).unwrap();
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .unwrap();
    doc.get_dictionary(root).unwrap().get(b"AcroForm").is_ok()
}

fn flatten_file(
    workspace: &Workspace,
    doc: lopdf::Document,
    pages: &str,
    options: FlattenOptions,
) -> (pdf_core::FlattenReport, std::path::PathBuf) {
    let input = workspace.write("form.pdf", doc);
    let mut loaded = Document::open(&input).unwrap();
    let report = pdf_core::flatten(&mut loaded, &range(pages), options).unwrap();
    let out = workspace.join("flat.pdf");
    loaded.save(&out).unwrap();
    (report, out)
}

#[test]
fn flattening_a_field_draws_its_appearance_and_removes_the_widget() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    add_annotation(
        &mut doc,
        1,
        "Widget",
        [100, 200, 200, 240],
        Normal::Text("Hello field"),
        4,
    );

    let (report, out) = flatten_file(&workspace, doc, "1", FlattenOptions::default());

    assert_eq!(report.pages_flattened, 1);
    assert_eq!(report.widgets_flattened, 1);
    assert_eq!(report.annots_flattened, 0);
    assert_eq!(report.skipped, 0);
    assert!(report.acroform_removed);

    assert_eq!(annot_count(&out, 1), 0);
    assert!(!has_acroform(&out));
    assert_eq!(drawn_appearances(&out, 1), ["Hello field"]);
    // The page's own text is untouched.
    assert_eq!(common::page_labels(&out), ["Page 1"]);

    // The 50x20 appearance is scaled to fill the 100x40 rectangle at (100, 200).
    let ops = page_operations(&out, 1);
    let cm = ops.iter().rev().find(|op| op.operator == "cm").unwrap();
    let numbers: Vec<f32> = cm.operands.iter().map(|o| o.as_float().unwrap()).collect();
    assert_eq!(numbers, [2.0, 0.0, 0.0, 2.0, 100.0, 200.0]);

    // Nothing is left of the field objects.
    let reloaded = lopdf::Document::load(&out).unwrap();
    assert!(!reloaded
        .objects
        .values()
        .any(|o| matches!(o, Object::Dictionary(d) if d.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Widget".as_slice()))));
}

#[test]
fn flattened_text_is_readable_by_another_reader() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    add_annotation(
        &mut doc,
        1,
        "Widget",
        [100, 200, 200, 240],
        Normal::Text("Typed answer"),
        4,
    );

    let (_, out) = flatten_file(&workspace, doc, "1", FlattenOptions::default());

    if let Some(text) = common::pdftotext(&out) {
        assert!(text.contains("Typed answer"), "poppler read: {text:?}");
        assert!(text.contains("Page 1"));
    }
}

#[test]
fn a_checkbox_draws_the_state_it_is_showing() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    add_annotation(
        &mut doc,
        1,
        "Widget",
        [10, 10, 60, 30],
        Normal::States {
            showing: "Yes",
            on: "CHECKED",
            off: "EMPTY",
        },
        4,
    );
    add_annotation(
        &mut doc,
        1,
        "Widget",
        [10, 50, 60, 70],
        Normal::States {
            showing: "Off",
            on: "CHECKED",
            off: "EMPTY",
        },
        4,
    );

    let (report, out) = flatten_file(&workspace, doc, "1", FlattenOptions::default());

    assert_eq!(report.widgets_flattened, 2);
    assert_eq!(drawn_appearances(&out, 1), ["CHECKED", "EMPTY"]);
}

#[test]
fn forms_only_keeps_other_annotations_and_the_rest_flatten_without_the_form() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    add_annotation(
        &mut doc,
        1,
        "Widget",
        [10, 10, 60, 30],
        Normal::Text("field"),
        4,
    );
    add_annotation(
        &mut doc,
        1,
        "Stamp",
        [10, 50, 60, 70],
        Normal::Text("approved"),
        4,
    );

    let (report, out) = flatten_file(
        &workspace,
        doc,
        "1",
        FlattenOptions {
            forms_only: true,
            annots_only: false,
        },
    );

    assert_eq!(report.widgets_flattened, 1);
    assert_eq!(report.annots_flattened, 0);
    assert_eq!(annot_count(&out, 1), 1);
    assert_eq!(drawn_appearances(&out, 1), ["field"]);
    // No widget is left, so the form goes.
    assert!(report.acroform_removed);
    assert!(!has_acroform(&out));
}

#[test]
fn annots_only_leaves_widgets_and_the_form_alone() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    add_annotation(
        &mut doc,
        1,
        "Widget",
        [10, 10, 60, 30],
        Normal::Text("field"),
        4,
    );
    add_annotation(
        &mut doc,
        1,
        "Stamp",
        [10, 50, 60, 70],
        Normal::Text("approved"),
        4,
    );

    let (report, out) = flatten_file(
        &workspace,
        doc,
        "1",
        FlattenOptions {
            forms_only: false,
            annots_only: true,
        },
    );

    assert_eq!(report.widgets_flattened, 0);
    assert_eq!(report.annots_flattened, 1);
    assert!(!report.acroform_removed);
    assert_eq!(annot_count(&out, 1), 1);
    assert!(has_acroform(&out));
    assert_eq!(drawn_appearances(&out, 1), ["approved"]);
}

#[test]
fn asking_for_forms_only_and_annots_only_together_is_refused() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    let error = pdf_core::flatten(
        &mut doc,
        &range("1"),
        FlattenOptions {
            forms_only: true,
            annots_only: true,
        },
    )
    .unwrap_err();
    assert!(
        matches!(error, PdfError::InvalidFlattenOptions),
        "got {error:?}"
    );
}

#[test]
fn annotations_without_an_appearance_stay_and_are_counted() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    add_annotation(&mut doc, 1, "Widget", [10, 10, 60, 30], Normal::None, 4);
    // A link never has an appearance; that is normal, not a problem.
    add_annotation(&mut doc, 1, "Link", [10, 50, 60, 70], Normal::None, 4);

    let (report, out) = flatten_file(&workspace, doc, "1", FlattenOptions::default());

    assert_eq!(report.skipped, 1);
    assert_eq!(report.widgets_flattened, 0);
    assert_eq!(report.pages_flattened, 0);
    assert_eq!(annot_count(&out, 1), 2);
    // The widget is still there, so the form must be too.
    assert!(!report.acroform_removed);
    assert!(has_acroform(&out));
}

#[test]
fn hidden_annotations_are_removed_without_being_drawn() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    add_annotation(
        &mut doc,
        1,
        "Stamp",
        [10, 10, 60, 30],
        Normal::Text("secret"),
        2,
    );
    add_annotation(
        &mut doc,
        1,
        "Stamp",
        [10, 50, 60, 70],
        Normal::Text("shown"),
        4,
    );

    let (report, out) = flatten_file(&workspace, doc, "1", FlattenOptions::default());

    assert_eq!(report.annots_flattened, 1);
    assert_eq!(annot_count(&out, 1), 0);
    assert_eq!(drawn_appearances(&out, 1), ["shown"]);
}

#[test]
fn flattening_one_page_leaves_widgets_elsewhere_and_keeps_the_form() {
    let workspace = Workspace::new();
    let mut doc = common::build(2, "Page", A4, Attributes::Inherited);
    add_annotation(
        &mut doc,
        1,
        "Widget",
        [10, 10, 60, 30],
        Normal::Text("one"),
        4,
    );
    add_annotation(
        &mut doc,
        2,
        "Widget",
        [10, 10, 60, 30],
        Normal::Text("two"),
        4,
    );

    let (report, out) = flatten_file(&workspace, doc, "1", FlattenOptions::default());

    assert_eq!(report.pages_flattened, 1);
    assert!(!report.acroform_removed);
    assert!(has_acroform(&out));
    assert_eq!(annot_count(&out, 1), 0);
    assert_eq!(annot_count(&out, 2), 1);
    assert_eq!(drawn_appearances(&out, 1), ["one"]);
    assert!(drawn_appearances(&out, 2).is_empty());
    // Page 2 inherited its resources; flattening page 1 must not have put an
    // XObject into the shared dictionary.
    assert_eq!(common::page_labels(&out), ["Page 1", "Page 2"]);
}

#[test]
fn flattening_isolates_the_pages_own_graphics_state() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);

    // A page whose content opens a transformation and never closes it.
    let page_id = doc.get_pages()[&1];
    let sloppy = lopdf::content::Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![2.into(), 0.into(), 0.into(), 2.into(), 0.into(), 0.into()],
            ),
        ],
    };
    let sloppy_id = doc.add_object(Stream::new(dictionary! {}, sloppy.encode().unwrap()));
    doc.get_dictionary_mut(page_id)
        .unwrap()
        .set("Contents", Object::Reference(sloppy_id));
    add_annotation(&mut doc, 1, "Stamp", [10, 10, 60, 30], Normal::Text("x"), 4);

    let (_, out) = flatten_file(&workspace, doc, "1", FlattenOptions::default());

    // Depth never goes below zero, and what the annotation draws starts at the
    // outermost level, not inside the page's own unclosed `q`.
    let mut depth = 0i32;
    let mut at_do = None;
    for op in page_operations(&out, 1) {
        match op.operator.as_str() {
            "q" => depth += 1,
            "Q" => depth -= 1,
            "Do" => at_do = Some(depth),
            _ => {}
        }
        assert!(depth >= 0);
    }
    assert_eq!(
        at_do,
        Some(1),
        "the annotation should be drawn one level deep, inside its own q"
    );
    assert_eq!(depth, 0, "q and Q must balance");
}

#[test]
fn an_xfa_form_is_refused() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", A4, Attributes::OnPage);
    add_annotation(
        &mut doc,
        1,
        "Widget",
        [10, 10, 60, 30],
        Normal::Text("x"),
        4,
    );
    // Make the form indirect, as producers usually do, and give it an XFA entry.
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .unwrap();
    let mut form = doc
        .get_dictionary(root)
        .unwrap()
        .get(b"AcroForm")
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    form.set("XFA", Object::Array(vec![]));
    let form_id = doc.add_object(form);
    doc.get_dictionary_mut(root)
        .unwrap()
        .set("AcroForm", Object::Reference(form_id));

    let input = workspace.write("xfa.pdf", doc);
    let mut loaded = Document::open(&input).unwrap();
    let error = pdf_core::flatten(&mut loaded, &range("1"), FlattenOptions::default()).unwrap_err();
    assert!(matches!(error, PdfError::XfaForm), "got {error:?}");
}

#[test]
fn flattening_a_document_with_no_annotations_changes_nothing() {
    let workspace = Workspace::new();
    let doc = common::build(2, "Page", A4, Attributes::OnPage);

    let (report, out) = flatten_file(&workspace, doc, "1-2", FlattenOptions::default());

    assert_eq!(report, pdf_core::FlattenReport::default());
    assert_eq!(common::page_labels(&out), ["Page 1", "Page 2"]);
}

#[test]
fn flattening_an_out_of_range_page_is_reported() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    let error = pdf_core::flatten(&mut doc, &range("5"), FlattenOptions::default()).unwrap_err();
    assert!(
        matches!(error, PdfError::PageOutOfRange { .. }),
        "got {error:?}"
    );
}

// -------------------------------------------------- crop, resize and scale

/// A page's own box entry as numbers, read from a fresh parse.
fn page_box(path: &std::path::Path, page_number: u32, key: &[u8]) -> Option<Vec<f32>> {
    let doc = lopdf::Document::load(path).unwrap();
    let page_id = doc.get_pages()[&page_number];
    let values = doc.get_dictionary(page_id).unwrap().get(key).ok()?;
    let values = doc.dereference(values).unwrap().1.as_array().unwrap();
    Some(
        values
            .iter()
            .map(|value| match value {
                Object::Integer(number) => *number as f32,
                Object::Real(number) => *number,
                other => panic!("box entry is not a number: {other:?}"),
            })
            .collect(),
    )
}

fn save_as(workspace: &Workspace, doc: &mut Document, name: &str) -> std::path::PathBuf {
    let out = workspace.join(name);
    doc.save(&out).unwrap();
    out
}

#[test]
fn crop_sets_a_crop_box_and_clamps_it_to_the_media_box() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    // Hangs off the left and top of an A4 page.
    let rect = pdf_core::Rect::from_xywh(-50.0, 100.0, 300.0, 2000.0).unwrap();
    pdf_core::crop(&mut doc, &range("1"), rect).unwrap();
    let out = save_as(&workspace, &mut doc, "out.pdf");

    assert_eq!(
        page_box(&out, 1, b"CropBox").unwrap(),
        [0.0, 100.0, 250.0, 842.0]
    );
    assert_eq!(common::page_labels(&out), ["Page 1"]);
}

#[test]
fn crop_leaves_unselected_pages_alone_when_the_media_box_is_inherited() {
    let workspace = Workspace::new();
    let doc = common::build(2, "Page", A4, Attributes::Inherited);
    let input = workspace.write("in.pdf", doc);
    let mut doc = Document::open(&input).unwrap();

    let rect = pdf_core::Rect::from_xywh(10.0, 10.0, 100.0, 100.0).unwrap();
    pdf_core::crop(&mut doc, &range("1"), rect).unwrap();
    let out = save_as(&workspace, &mut doc, "out.pdf");

    assert_eq!(
        page_box(&out, 1, b"CropBox").unwrap(),
        [10.0, 10.0, 110.0, 110.0]
    );
    assert_eq!(page_box(&out, 2, b"CropBox"), None);
    // Both pages still know their size after the tree was flattened.
    assert_eq!(
        common::page_media_boxes(&out),
        vec![vec![0, 0, 595, 842]; 2]
    );
}

#[test]
fn crop_outside_the_page_is_refused() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    let rect = pdf_core::Rect::from_xywh(5000.0, 5000.0, 10.0, 10.0).unwrap();
    let error = pdf_core::crop(&mut doc, &range("1"), rect).unwrap_err();
    assert!(
        matches!(error, PdfError::InvalidCropBox { .. }),
        "got {error:?}"
    );
}

#[test]
fn reset_crop_removes_the_crop_box() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 2, "Page");
    let mut doc = Document::open(&input).unwrap();

    let rect = pdf_core::Rect::from_xywh(10.0, 10.0, 100.0, 100.0).unwrap();
    pdf_core::crop(&mut doc, &range("1-2"), rect).unwrap();
    pdf_core::reset_crop(&mut doc, &range("1")).unwrap();
    let out = save_as(&workspace, &mut doc, "out.pdf");

    assert_eq!(page_box(&out, 1, b"CropBox"), None);
    assert!(page_box(&out, 2, b"CropBox").is_some());
}

#[test]
fn set_mediabox_pulls_an_existing_crop_box_back_inside() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    let crop = pdf_core::Rect::from_xywh(0.0, 0.0, 500.0, 500.0).unwrap();
    pdf_core::crop(&mut doc, &range("1"), crop).unwrap();

    let media = pdf_core::Rect::from_xywh(0.0, 0.0, 300.0, 400.0).unwrap();
    pdf_core::set_mediabox(&mut doc, &range("1"), media).unwrap();
    let out = save_as(&workspace, &mut doc, "out.pdf");

    assert_eq!(
        page_box(&out, 1, b"MediaBox").unwrap(),
        [0.0, 0.0, 300.0, 400.0]
    );
    assert_eq!(
        page_box(&out, 1, b"CropBox").unwrap(),
        [0.0, 0.0, 300.0, 400.0]
    );
}

#[test]
fn scale_to_a4_resizes_the_page_and_wraps_its_content() {
    let workspace = Workspace::new();
    let doc = common::build(1, "Page", LETTER, Attributes::OnPage);
    let input = workspace.write("in.pdf", doc);
    let mut doc = Document::open(&input).unwrap();

    pdf_core::scale_to(
        &mut doc,
        &range("1"),
        pdf_core::PageSize::A4,
        pdf_core::Fit::Contain,
    )
    .unwrap();
    let out = save_as(&workspace, &mut doc, "out.pdf");

    let media = page_box(&out, 1, b"MediaBox").unwrap();
    assert!((media[2] - 595.276).abs() < 0.01 && (media[3] - 841.89).abs() < 0.01);

    let operators: Vec<String> = page_operations(&out, 1)
        .into_iter()
        .map(|op| op.operator)
        .collect();
    assert_eq!(&operators[..2], ["q", "cm"]);
    assert_eq!(operators.last().unwrap(), "Q");
    assert_eq!(common::page_labels(&out), ["Page 1"]);
}

#[test]
fn scale_to_keeps_the_displayed_size_of_a_rotated_page() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Page", LETTER, Attributes::OnPage);
    let page_id = doc.get_pages()[&1];
    doc.get_dictionary_mut(page_id)
        .unwrap()
        .set("Rotate", Object::Integer(90));
    let input = workspace.write("in.pdf", doc);
    let mut doc = Document::open(&input).unwrap();

    pdf_core::scale_to(
        &mut doc,
        &range("1"),
        pdf_core::PageSize::A4,
        pdf_core::Fit::Stretch,
    )
    .unwrap();
    let out = save_as(&workspace, &mut doc, "out.pdf");

    // Stored on its side so that, turned 90 degrees, it displays as portrait A4.
    let media = page_box(&out, 1, b"MediaBox").unwrap();
    assert!((media[2] - 841.89).abs() < 0.01 && (media[3] - 595.276).abs() < 0.01);
}

#[test]
fn scale_to_fit_image_is_refused() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    let error = pdf_core::scale_to(
        &mut doc,
        &range("1"),
        pdf_core::PageSize::FitImage,
        pdf_core::Fit::Contain,
    )
    .unwrap_err();
    assert!(
        matches!(error, PdfError::InvalidMediaBox { .. }),
        "got {error:?}"
    );
}

// ------------------------------------------------------------------ filter

fn first_image_pixels(path: &std::path::Path) -> image::RgbImage {
    let doc = lopdf::Document::load(path).unwrap();
    for object in doc.objects.values() {
        if let Object::Stream(stream) = object {
            let is_image = stream
                .dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|name| name == b"Image");
            if is_image {
                return image::load_from_memory(&stream.content).unwrap().to_rgb8();
            }
        }
    }
    panic!("no image in {path:?}");
}

#[test]
fn inverting_a_jpeg_flips_its_pixels() {
    let workspace = Workspace::new();
    let doc = common::build_with_image(32, 32, common::ImagePlacement::drawn(200.0, 200.0));
    let input = workspace.write("in.pdf", doc);
    let before = first_image_pixels(&input);

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::filter(&mut doc, &range("1"), pdf_core::Filter::Invert).unwrap();
    assert_eq!((report.images_examined, report.images_filtered), (1, 1));
    assert_eq!(report.images_skipped(), 0);
    let out = save_as(&workspace, &mut doc, "out.pdf");

    let after = first_image_pixels(&out);
    assert_eq!(after.dimensions(), before.dimensions());
    // JPEG is lossy, so allow for rounding in both directions.
    let total: i64 = before
        .pixels()
        .zip(after.pixels())
        .flat_map(|(b, a)| (0..3).map(move |c| (i64::from(255 - b[c]) - i64::from(a[c])).abs()))
        .sum();
    let average = total as f64 / (32.0 * 32.0 * 3.0);
    assert!(average < 12.0, "average error {average}");
}

#[test]
fn grayscale_removes_the_colour_from_an_image() {
    let workspace = Workspace::new();
    let doc = common::build_with_image(32, 32, common::ImagePlacement::drawn(200.0, 200.0));
    let input = workspace.write("in.pdf", doc);

    let mut doc = Document::open(&input).unwrap();
    let report = pdf_core::filter(&mut doc, &range("1"), pdf_core::Filter::Grayscale).unwrap();
    assert_eq!(report.images_filtered, 1);
    let out = save_as(&workspace, &mut doc, "out.pdf");

    let after = first_image_pixels(&out);
    let worst = after
        .pixels()
        .map(|p| {
            let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
            (r - g).abs().max((g - b).abs())
        })
        .max()
        .unwrap();
    assert!(worst <= 8, "channels differ by {worst}");
}

#[test]
fn filtering_a_page_with_no_images_reports_nothing_to_do() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    let report = pdf_core::filter(&mut doc, &range("1"), pdf_core::Filter::Invert).unwrap();
    assert_eq!(report.images_examined, 0);
    assert_eq!(report.images_filtered, 0);
}

#[test]
fn an_unknown_filter_name_is_refused() {
    let error = pdf_core::Filter::parse("sepia").unwrap_err();
    assert!(matches!(error, PdfError::InvalidFilter(name) if name == "sepia"));
    assert_eq!(
        pdf_core::Filter::parse("Grey").unwrap(),
        pdf_core::Filter::Grayscale
    );
}

// ------------------------------------------------------------------ redact

fn page_text_runs(path: &std::path::Path, page_number: u32) -> Vec<String> {
    page_operations(path, page_number)
        .iter()
        .filter(|op| op.operator == "Tj")
        .filter_map(|op| match op.operands.first() {
            Some(Object::String(bytes, _)) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn redacting_an_area_draws_a_box_and_removes_the_text() {
    let workspace = Workspace::new();
    let input = workspace.write("in.pdf", common::build_text_page("Account 12345", 12.0));
    let mut doc = Document::open(&input).unwrap();

    let rect = pdf_core::Rect::from_xywh(60.0, 590.0, 200.0, 30.0).unwrap();
    let report = pdf_core::redact(&mut doc, &range("1"), rect).unwrap();
    assert_eq!(report.pages_redacted, 1);
    assert_eq!(report.boxes_drawn, 1);
    assert_eq!(report.runs_removed, 1);
    let out = save_as(&workspace, &mut doc, "out.pdf");

    assert!(page_text_runs(&out, 1).is_empty());
    let operators: Vec<String> = page_operations(&out, 1)
        .into_iter()
        .map(|op| op.operator)
        .collect();
    assert!(operators.iter().any(|op| op == "re"));
    assert!(operators.iter().any(|op| op == "f"));
    // The words are gone from the raw bytes too, not just hidden.
    let bytes = std::fs::read(&out).unwrap();
    assert!(!bytes.windows(5).any(|window| window == b"12345"));

    if let Some(text) = common::pdftotext(&out) {
        assert!(!text.contains("12345"), "{text:?}");
    }
}

#[test]
fn redact_find_removes_only_the_matching_text() {
    let workspace = Workspace::new();
    let input = workspace.write("in.pdf", common::build(2, "Page", A4, Attributes::OnPage));
    let mut doc = Document::open(&input).unwrap();

    let report = pdf_core::redact_find(&mut doc, &range("1-2"), "Page 2").unwrap();
    assert_eq!(report.runs_removed, 1);
    assert_eq!(report.pages_redacted, 1);
    assert_eq!(report.boxes_drawn, 0);
    let out = save_as(&workspace, &mut doc, "out.pdf");

    assert_eq!(page_text_runs(&out, 1), ["Page 1"]);
    assert!(page_text_runs(&out, 2).is_empty());
}

#[test]
fn redact_find_reports_text_that_is_not_there() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    let error = pdf_core::redact_find(&mut doc, &range("1"), "absent").unwrap_err();
    assert!(matches!(error, PdfError::TextNotFound(text) if text == "absent"));
}

#[test]
fn redact_find_refuses_an_empty_search() {
    let workspace = Workspace::new();
    let input = workspace.document("in.pdf", 1, "Page");
    let mut doc = Document::open(&input).unwrap();

    let error = pdf_core::redact_find(&mut doc, &range("1"), "").unwrap_err();
    assert!(matches!(error, PdfError::EmptySearchText), "got {error:?}");
}

// ----------------------------------------------------- repair edge cases

#[test]
fn repairing_a_missing_file_says_so() {
    let workspace = Workspace::new();
    let error =
        pdf_core::repair(&workspace.join("absent.pdf"), &workspace.join("out.pdf")).unwrap_err();
    assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
}

#[test]
fn repairing_an_encrypted_file_is_refused() {
    let workspace = Workspace::new();
    let input = workspace.document("plain.pdf", 1, "Page");
    let locked = workspace.join("locked.pdf");
    Document::open(&input)
        .unwrap()
        .save_encrypted(
            &locked,
            &pdf_core::EncryptOptions {
                user_password: "user".into(),
                owner_password: "owner".into(),
                ..pdf_core::EncryptOptions::default()
            },
        )
        .unwrap();

    let error = pdf_core::repair(&locked, &workspace.join("out.pdf")).unwrap_err();
    assert!(matches!(error, PdfError::Encrypted(_)), "got {error:?}");
}

// ----------------------------------------------------------------- encrypt

#[test]
fn encrypt_permissions_and_cleartext_metadata_are_honoured() {
    let workspace = Workspace::new();
    let input = workspace.document("plain.pdf", 1, "Page");
    let out = workspace.join("locked.pdf");

    let mut doc = Document::open(&input).unwrap();
    doc.save_encrypted(
        &out,
        &pdf_core::EncryptOptions {
            user_password: "user".into(),
            owner_password: "owner".into(),
            permissions: pdf_core::Permissions::PRINTABLE,
            encrypt_metadata: false,
        },
    )
    .unwrap();

    // The permission word lands in /P: printing allowed, copying not.
    let raw = lopdf::Document::load(&out).unwrap();
    let encrypt = raw
        .trailer
        .get(b"Encrypt")
        .and_then(Object::as_reference)
        .unwrap();
    let dict = raw.get_dictionary(encrypt).unwrap();
    let permissions = dict.get(b"P").and_then(Object::as_i64).unwrap() as i32 as u32 as u64;
    assert_ne!(permissions & pdf_core::Permissions::PRINTABLE.bits(), 0);
    assert_eq!(permissions & pdf_core::Permissions::COPYABLE.bits(), 0);

    let reopened = Document::open_with_password(&out, "user").unwrap();
    assert_eq!(reopened.page_count(), 1);
}
