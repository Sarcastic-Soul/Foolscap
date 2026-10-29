//! End-to-end PDF/A tests: convert a generated document, write it to disk,
//! reload it, and check it — what a validator would do to the real file.

mod common;

use common::{build_with_image, Attributes, ImagePlacement, Workspace, A4};
use pdf_core::ops::pdfa::check_pdfa;
use pdf_core::{to_pdfa, Document, PdfALevel};

fn issue(report: &pdf_core::PdfAReport, needle: &str) -> bool {
    report.issues.iter().any(|issue| issue.contains(needle))
}

#[test]
fn converted_output_passes_the_check_after_a_round_trip() {
    let workspace = Workspace::new();

    // An image-only document: no fonts to embed, and a plain JPEG is valid at
    // every PDF/A level, so what is under test is the conversion itself.
    let original = workspace.write(
        "photo.pdf",
        build_with_image(60, 40, ImagePlacement::drawn(200.0, 150.0)),
    );

    // The unconverted original fails: no OutputIntent, no XMP identification.
    let doc = Document::open(&original).unwrap();
    let report = check_pdfa(&doc).unwrap();
    assert!(!report.conformant, "issues: {:?}", report.issues);
    assert!(issue(&report, "OutputIntent"));
    assert!(issue(&report, "XMP"));

    for (level, name) in [
        (PdfALevel::A1b, "converted-1b.pdf"),
        (PdfALevel::A2b, "converted-2b.pdf"),
        (PdfALevel::A3b, "converted-3b.pdf"),
    ] {
        let mut doc = Document::open(&original).unwrap();
        let report = to_pdfa(&mut doc, level).unwrap();
        assert!(report.conformant, "{name}: issues: {:?}", report.issues);

        let out = workspace.join(name);
        doc.save(&out).unwrap();

        // Reload from disk: streams are compressed now, so the XMP has to be
        // decompressed on the way in, and every reference goes through the xref.
        let doc = Document::open(&out).unwrap();
        let report = check_pdfa(&doc).unwrap();
        assert!(report.conformant, "{name}: issues: {:?}", report.issues);
    }
}

#[test]
fn pdfa1_output_is_pinned_to_pdf_1_4() {
    let workspace = Workspace::new();
    // The generator writes PDF 1.5, which PDF/A-1 cannot keep.
    let original = workspace.write(
        "photo.pdf",
        build_with_image(60, 40, ImagePlacement::drawn(200.0, 150.0)),
    );

    let mut doc = Document::open(&original).unwrap();
    to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
    let out = workspace.join("pinned.pdf");
    doc.save(&out).unwrap();

    let doc = Document::open(&out).unwrap();
    assert_eq!(doc.version(), "1.4");
    assert!(check_pdfa(&doc).unwrap().conformant);
}

#[test]
fn unembedded_fonts_are_flagged_wherever_the_resources_sit() {
    let workspace = Workspace::new();

    for attributes in [Attributes::OnPage, Attributes::Inherited] {
        // `build` draws with Courier, a base-14 font no font program ships for.
        let path = workspace.write("text.pdf", common::build(2, "Page", A4, attributes));

        let doc = Document::open(&path).unwrap();
        let report = check_pdfa(&doc).unwrap();
        assert!(
            issue(&report, "Courier"),
            "{attributes:?}: issues: {:?}",
            report.issues
        );

        // PDF/A-1b refuses to convert; PDF/A-2b converts but stays honest
        // about the fonts it could not embed.
        let mut doc = Document::open(&path).unwrap();
        assert!(to_pdfa(&mut doc, PdfALevel::A1b).is_err());

        let mut doc = Document::open(&path).unwrap();
        let report = to_pdfa(&mut doc, PdfALevel::A2b).unwrap();
        assert_eq!(report.unembedded_fonts, ["Courier"]);
        assert!(!report.conformant);
        assert!(issue(&report, "Courier"));

        let out = workspace.join("text-2b.pdf");
        doc.save(&out).unwrap();
        let report = check_pdfa(&Document::open(&out).unwrap()).unwrap();
        assert!(!report.conformant);
        assert!(issue(&report, "Courier"));
    }
}

#[test]
fn verapdf_validates_the_converted_output() {
    // verapdf is the reference validator; like the LibreOffice and Tesseract
    // tests, this skips itself when the tool is not installed.
    let probe = std::process::Command::new("verapdf").arg("--help").output();
    let Ok(output) = probe else {
        eprintln!("skipping the verapdf check: verapdf is not on PATH");
        return;
    };
    assert!(output.status.success(), "verapdf --help failed");

    let workspace = Workspace::new();
    let original = workspace.write(
        "photo.pdf",
        build_with_image(60, 40, ImagePlacement::drawn(200.0, 150.0)),
    );
    let mut doc = Document::open(&original).unwrap();
    to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
    let out = workspace.join("verapdf.pdf");
    doc.save(&out).unwrap();

    let output = std::process::Command::new("verapdf")
        .arg("--flavour")
        .arg("1b")
        .arg(&out)
        .output()
        .expect("verapdf ran above");
    assert!(
        output.status.success(),
        "verapdf rejected the converted file:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
