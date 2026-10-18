//! Redaction tests: text inside Form XObjects, text split at the box, and
//! image pixels, each checked on the saved file rather than on the in-memory
//! document.

mod common;

use std::io::Write;
use std::path::Path;

use common::Workspace;
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Object, ObjectId, Stream};
use pdf_core::{Document, PageRange, Rect};

fn range(spec: &str) -> PageRange {
    PageRange::parse(spec).unwrap()
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_xywh(x, y, w, h).unwrap()
}

// ------------------------------------------------------------------ fixtures

/// Builds a document one object at a time. Pages are added last, with
/// `finish`.
struct Builder {
    doc: lopdf::Document,
    pages_id: ObjectId,
    font_id: ObjectId,
    kids: Vec<Object>,
}

impl Builder {
    fn new() -> Self {
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        Self {
            doc,
            pages_id,
            font_id,
            kids: Vec::new(),
        }
    }

    fn font_resources(&self) -> Dictionary {
        dictionary! { "F1" => self.font_id }
    }

    fn form(&mut self, content: &str, matrix: [i64; 6], xobjects: Dictionary) -> ObjectId {
        let id = self.doc.new_object_id();
        self.set_form(id, content, matrix, xobjects);
        id
    }

    fn set_form(&mut self, id: ObjectId, content: &str, matrix: [i64; 6], xobjects: Dictionary) {
        let stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
                "Matrix" => matrix.iter().map(|v| Object::Integer(*v)).collect::<Vec<_>>(),
                "Resources" => dictionary! {
                    "Font" => self.font_resources(),
                    "XObject" => xobjects,
                },
            },
            content.as_bytes().to_vec(),
        );
        self.doc.objects.insert(id, Object::Stream(stream));
    }

    fn page(&mut self, content: &str, xobjects: Dictionary) {
        let content_id = self
            .doc
            .add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
        let page = dictionary! {
            "Type" => "Page",
            "Parent" => self.pages_id,
            "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => dictionary! {
                "Font" => self.font_resources(),
                "XObject" => xobjects,
            },
        };
        let id = self.doc.add_object(page);
        self.kids.push(Object::Reference(id));
    }

    fn finish(mut self, workspace: &Workspace, name: &str) -> std::path::PathBuf {
        self.doc.objects.insert(
            self.pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Count" => self.kids.len() as i64,
                "Kids" => self.kids.clone(),
            }),
        );
        let catalog = self.doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => self.pages_id,
        });
        self.doc.trailer.set("Root", catalog);
        workspace.write(name, self.doc)
    }
}

/// A page showing a heading, and a form (scaled by two) with text, a nested
/// form, and a line at the bottom. Where things land on the page:
///
/// - "Public heading", 12pt at (72, 780)
/// - "Hello SECRET world", 24pt, baseline y=600, "SECRET" from x=181 to 279
/// - "Nested HIDDEN2", 24pt, baseline y=500
/// - "Far away keep", 24pt, baseline y=200
fn form_document(workspace: &Workspace) -> std::path::PathBuf {
    let mut b = Builder::new();
    let nested = b.form(
        "BT /F1 12 Tf 10 400 Td (Nested HIDDEN2) Tj ET",
        [1, 0, 0, 1, 0, 0],
        dictionary! {},
    );
    let outer = b.form(
        "BT /F1 12 Tf 10 300 Td (Hello SECRET world) Tj ET\n\
         q 1 0 0 1 0 -150 cm /Fm2 Do Q\n\
         BT /F1 12 Tf 10 100 Td (Far away keep) Tj ET",
        [2, 0, 0, 2, 0, 0],
        dictionary! { "Fm2" => nested },
    );
    b.page(
        "BT /F1 12 Tf 72 780 Td (Public heading) Tj ET\nq 1 0 0 1 100 0 cm /Fm1 Do Q",
        dictionary! { "Fm1" => outer },
    );
    b.finish(workspace, "forms.pdf")
}

fn redacted(
    workspace: &Workspace,
    input: &Path,
    pages: &str,
    area: Rect,
    name: &str,
) -> (std::path::PathBuf, pdf_core::RedactReport) {
    let mut doc = Document::open(input).unwrap();
    let report = pdf_core::redact(&mut doc, &range(pages), area).unwrap();
    let out = workspace.join(name);
    doc.save(&out).unwrap();
    (out, report)
}

fn has_bytes(path: &Path, needle: &str) -> bool {
    let bytes = std::fs::read(path).unwrap();
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Poppler's text for each page, or `None` when it is not installed.
fn pages_text(path: &Path) -> Option<Vec<String>> {
    common::pdftotext(path).map(|text| text.split('\x0c').map(str::to_string).collect())
}

#[cfg(feature = "render")]
fn mupdf_text(path: &Path, page: usize) -> String {
    pdf_core::render::PageRenderer::open(path)
        .unwrap()
        .page_text(page)
        .unwrap()
}

fn image_streams(path: &Path) -> Vec<Stream> {
    let doc = lopdf::Document::load(path).unwrap();
    doc.objects
        .values()
        .filter_map(|object| match object {
            Object::Stream(stream)
                if stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                    == Some(b"Image".as_slice()) =>
            {
                Some(stream.clone())
            }
            _ => None,
        })
        .collect()
}

fn page_operations(path: &Path, page: u32) -> Vec<Operation> {
    let doc = lopdf::Document::load(path).unwrap();
    let id = doc.get_pages()[&page];
    Content::decode(&doc.get_page_content(id))
        .unwrap()
        .operations
}

// -------------------------------------------------------------- form text

#[test]
fn text_in_a_form_xobject_is_removed_and_the_rest_kept() {
    let workspace = Workspace::new();
    let input = form_document(&workspace);

    // Covers "SECRET" (x 181 to 279 at baseline 600) but not its neighbours.
    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(176.0, 590.0, 107.0, 40.0),
        "out.pdf",
    );

    assert_eq!(report.forms_edited, 1);
    assert_eq!(report.runs_trimmed, 1);
    assert_eq!(report.runs_removed, 0);
    // "SECRET" is six glyphs, plus the spaces either side that the box clips.
    assert!(report.glyphs_removed >= 6, "{report:?}");

    assert!(!has_bytes(&out, "SECRET"));
    if let Some(pages) = pages_text(&out) {
        let text = &pages[0];
        assert!(!text.contains("SECRET"), "{text:?}");
        for kept in [
            "Public heading",
            "Hello",
            "world",
            "Nested HIDDEN2",
            "Far away keep",
        ] {
            assert!(text.contains(kept), "{kept:?} missing from {text:?}");
        }
    }
    #[cfg(feature = "render")]
    {
        let text = mupdf_text(&out, 0);
        assert!(!text.contains("SECRET"), "{text:?}");
        assert!(text.contains("Hello") && text.contains("world"), "{text:?}");
    }
}

#[test]
fn text_in_a_nested_form_follows_both_matrices() {
    let workspace = Workspace::new();
    let input = form_document(&workspace);

    // Only the nested form's line sits at baseline 500.
    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(100.0, 490.0, 300.0, 40.0),
        "out.pdf",
    );

    // The nested form was edited, and so was the form that holds it.
    assert_eq!(report.forms_edited, 2);
    assert_eq!(report.runs_removed, 1);
    assert!(!has_bytes(&out, "HIDDEN2"));
    if let Some(pages) = pages_text(&out) {
        let text = &pages[0];
        assert!(!text.contains("HIDDEN2"), "{text:?}");
        for kept in ["Public heading", "Hello SECRET world", "Far away keep"] {
            assert!(text.contains(kept), "{kept:?} missing from {text:?}");
        }
    }
}

#[test]
fn a_box_that_misses_the_form_text_leaves_the_form_alone() {
    let workspace = Workspace::new();
    let input = form_document(&workspace);

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(10.0, 10.0, 50.0, 50.0),
        "out.pdf",
    );

    assert_eq!(report.forms_edited, 0);
    assert_eq!(report.glyphs_removed, 0);
    if let Some(pages) = pages_text(&out) {
        assert!(pages[0].contains("Hello SECRET world"), "{:?}", pages[0]);
    }
}

#[test]
fn a_form_shared_with_another_page_is_copied_not_edited() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let shared = b.form(
        "BT /F1 12 Tf 72 600 Td (Shared SECRET) Tj ET",
        [1, 0, 0, 1, 0, 0],
        dictionary! {},
    );
    for _ in 0..2 {
        b.page("/Fm1 Do", dictionary! { "Fm1" => shared });
    }
    let input = b.finish(&workspace, "shared.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(60.0, 590.0, 300.0, 30.0),
        "out.pdf",
    );

    assert_eq!(report.forms_edited, 1);
    if let Some(pages) = pages_text(&out) {
        assert!(!pages[0].contains("SECRET"), "page 1: {:?}", pages[0]);
        assert!(pages[1].contains("Shared SECRET"), "page 2: {:?}", pages[1]);
    }
    // Page two still has its text in the file; page one's copy has none.
    assert!(has_bytes(&out, "Shared SECRET"));
}

#[test]
fn a_form_used_twice_on_a_page_is_edited_only_where_it_is_covered() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let form = b.form(
        "BT /F1 12 Tf 0 0 Td (Twice SECRET) Tj ET",
        [1, 0, 0, 1, 0, 0],
        dictionary! {},
    );
    b.page(
        "q 1 0 0 1 72 700 cm /Fm1 Do Q\nq 1 0 0 1 72 300 cm /Fm1 Do Q",
        dictionary! { "Fm1" => form },
    );
    let input = b.finish(&workspace, "twice.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(60.0, 690.0, 300.0, 30.0),
        "out.pdf",
    );

    assert_eq!(report.forms_edited, 1);
    if let Some(pages) = pages_text(&out) {
        // Exactly one of the two copies survives.
        assert_eq!(
            pages[0].matches("Twice SECRET").count(),
            1,
            "{:?}",
            pages[0]
        );
    }
}

#[test]
fn a_form_that_draws_itself_does_not_hang() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let id = b.doc.new_object_id();
    b.set_form(
        id,
        "BT /F1 12 Tf 72 600 Td (Loop SECRET) Tj ET\n/Fm1 Do",
        [1, 0, 0, 1, 0, 0],
        dictionary! { "Fm1" => id },
    );
    b.page("/Fm1 Do", dictionary! { "Fm1" => id });
    let input = b.finish(&workspace, "cycle.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(60.0, 590.0, 300.0, 30.0),
        "out.pdf",
    );

    assert!(report.forms_edited >= 1);
    assert!(!has_bytes(&out, "SECRET"));
}

// --------------------------------------------------------- finer than runs

#[test]
fn only_the_glyphs_under_the_box_leave_a_run() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    // Helvetica 12pt from x=72: "Hello " ends at 102.7, "SECRET" at 151.3.
    b.page(
        "BT /F1 12 Tf 72 700 Td (Hello SECRET world) Tj ET",
        dictionary! {},
    );
    let input = b.finish(&workspace, "run.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(101.0, 690.0, 52.0, 25.0),
        "out.pdf",
    );

    assert_eq!(report.runs_trimmed, 1);
    assert_eq!(report.runs_removed, 0);
    assert!(!has_bytes(&out, "SECRET"));
    if let Some(pages) = pages_text(&out) {
        assert!(!pages[0].contains("SECRET"), "{:?}", pages[0]);
        assert!(pages[0].contains("Hello"), "{:?}", pages[0]);
        assert!(pages[0].contains("world"), "{:?}", pages[0]);
    }
    // "world" is held in place by a kerning number where SECRET was.
    let operations = page_operations(&out, 1);
    let array = operations
        .iter()
        .find(|op| op.operator == "TJ")
        .and_then(|op| op.operands.first())
        .and_then(|operand| operand.as_array().ok())
        .expect("the trimmed run is a TJ array");
    assert!(array
        .iter()
        .any(|item| matches!(item, Object::Real(_) | Object::Integer(_))));
}

#[test]
fn quote_operators_are_split_into_plain_operators() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    b.page(
        "BT /F1 12 Tf 14 TL 72 700 Td (first) Tj (second SECRET) ' ET",
        dictionary! {},
    );
    let input = b.finish(&workspace, "quote.pdf");

    // The second line sits at baseline 686.
    let (out, _) = redacted(
        &workspace,
        &input,
        "1",
        rect(100.0, 680.0, 200.0, 12.0),
        "out.pdf",
    );

    assert!(!has_bytes(&out, "SECRET"));
    if let Some(pages) = pages_text(&out) {
        assert!(pages[0].contains("first"), "{:?}", pages[0]);
        assert!(!pages[0].contains("SECRET"), "{:?}", pages[0]);
    }
}

#[test]
fn text_outside_the_box_survives_an_area_redaction() {
    let workspace = Workspace::new();
    let input = form_document(&workspace);

    // Unlike the first version, nothing outside the box loses its text.
    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(176.0, 590.0, 107.0, 40.0),
        "out.pdf",
    );
    assert_eq!(report.pages_redacted, 1);
    assert_eq!(report.boxes_drawn, 1);
    if let Some(pages) = pages_text(&out) {
        assert!(pages[0].contains("Public heading"), "{:?}", pages[0]);
    }
}

#[test]
fn redact_find_reaches_text_inside_forms() {
    let workspace = Workspace::new();
    let input = form_document(&workspace);
    let mut doc = Document::open(&input).unwrap();

    let report = pdf_core::redact_find(&mut doc, &range("1"), "SECRET").unwrap();
    let out = workspace.join("out.pdf");
    doc.save(&out).unwrap();

    assert_eq!(report.runs_removed, 1);
    assert_eq!(report.forms_edited, 1);
    assert_eq!(report.boxes_drawn, 0);
    assert!(!has_bytes(&out, "SECRET"));
    if let Some(pages) = pages_text(&out) {
        assert!(pages[0].contains("Public heading"), "{:?}", pages[0]);
        assert!(pages[0].contains("Far away keep"), "{:?}", pages[0]);
    }
}

#[test]
fn a_transformation_the_page_leaves_open_does_not_move_the_box() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    // The page ends with a translation still in force.
    b.page(
        "BT /F1 12 Tf 72 700 Td (Account 12345) Tj ET\n1 0 0 1 300 300 cm",
        dictionary! {},
    );
    let input = b.finish(&workspace, "open.pdf");

    let (out, _) = redacted(
        &workspace,
        &input,
        "1",
        rect(60.0, 690.0, 200.0, 30.0),
        "out.pdf",
    );

    let operations = page_operations(&out, 1);
    let depth_at_box = {
        let mut depth = 0i32;
        let mut at_box = None;
        for op in &operations {
            match op.operator.as_str() {
                "q" => depth += 1,
                "Q" => depth -= 1,
                "re" => at_box = Some(depth),
                _ => {}
            }
        }
        (at_box, depth)
    };
    // The page's own `cm` is undone before the box is drawn: the box sits in a
    // `q` of its own, outside the one that held the page's content.
    assert_eq!(depth_at_box, (Some(1), 0));
}

// ------------------------------------------------------------------- paths

#[test]
fn paths_wholly_inside_the_box_are_removed_and_larger_ones_kept() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    b.page(
        "0.9 g 0 0 595 842 re f\n0 g 120 650 30 10 re f\n100 100 m 500 100 l S",
        dictionary! {},
    );
    let input = b.finish(&workspace, "paths.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(100.0, 640.0, 100.0, 40.0),
        "out.pdf",
    );

    assert_eq!(report.paths_removed, 1);
    let operations = page_operations(&out, 1);
    let rectangles: Vec<_> = operations.iter().filter(|op| op.operator == "re").collect();
    // The page background, plus the redaction box.
    assert_eq!(rectangles.len(), 2);
    assert!(operations.iter().any(|op| op.operator == "S"));
}

// ------------------------------------------------------------------ images

const SIDE: u32 = 100;

fn pixel(x: u32, y: u32) -> [u8; 3] {
    [200, 20 + x as u8, 50 + y as u8]
}

fn rgb_samples() -> Vec<u8> {
    let mut samples = Vec::new();
    for y in 0..SIDE {
        for x in 0..SIDE {
            samples.extend_from_slice(&pixel(x, y));
        }
    }
    samples
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn inflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn rgb_image(b: &mut Builder) -> ObjectId {
    b.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => SIDE as i64,
            "Height" => SIDE as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
        },
        deflate(&rgb_samples()),
    ))
}

/// The image drawn 200pt square with its lower left at (72, 500): one pixel
/// is two points. A box over x 172..272, y 600..700 covers the top right
/// quarter, pixels x 50.. and rows ..50.
const IMAGE_PAGE: &str =
    "q 200 0 0 200 72 500 cm /Im1 Do Q\nBT /F1 12 Tf 72 400 Td (caption) Tj ET";

fn decode_rgb(stream: &Stream) -> Vec<u8> {
    inflate(&stream.content)
}

#[test]
fn image_pixels_under_the_box_are_blanked_in_the_data() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let image = rgb_image(&mut b);
    b.page(IMAGE_PAGE, dictionary! { "Im1" => image });
    let input = b.finish(&workspace, "image.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(172.0, 600.0, 100.0, 100.0),
        "out.pdf",
    );

    assert_eq!(report.images_blanked, 1);
    assert_eq!(report.images_removed, 0);
    let images = image_streams(&out);
    // The unedited original is not left in the file.
    assert_eq!(images.len(), 1);
    let samples = decode_rgb(&images[0]);
    assert_eq!(samples.len(), (SIDE * SIDE * 3) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let at = ((y * SIDE + x) * 3) as usize;
            let got = [samples[at], samples[at + 1], samples[at + 2]];
            let covered = x >= 50 && y < 50;
            let want = if covered { [0, 0, 0] } else { pixel(x, y) };
            assert_eq!(got, want, "pixel ({x}, {y})");
        }
    }
    if let Some(pages) = pages_text(&out) {
        assert!(pages[0].contains("caption"), "{:?}", pages[0]);
    }
}

#[cfg(feature = "render")]
#[test]
fn a_rendered_page_shows_black_where_the_image_was_and_picture_elsewhere() {
    use pdf_core::render::{PageRenderer, Scale};

    let workspace = Workspace::new();
    let mut b = Builder::new();
    let image = rgb_image(&mut b);
    b.page(IMAGE_PAGE, dictionary! { "Im1" => image });
    let input = b.finish(&workspace, "image.pdf");
    let (out, _) = redacted(
        &workspace,
        &input,
        "1",
        rect(172.0, 600.0, 100.0, 100.0),
        "out.pdf",
    );

    let page = PageRenderer::open(&out)
        .unwrap()
        .render(0, Scale::Dpi(72.0))
        .unwrap();
    let at = |x: usize, y_from_bottom: usize| {
        let row = page.height as usize - 1 - y_from_bottom;
        let start = row * page.stride() + x * page.channels as usize;
        page.pixels[start..start + 3].to_vec()
    };
    // Inside the box: black. The lower left of the picture: still the picture.
    assert_eq!(at(220, 650), [0, 0, 0]);
    let kept = at(100, 520);
    assert!(kept[0] > 150, "{kept:?}");
}

#[test]
fn an_image_shared_with_another_page_is_copied_not_edited() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let image = rgb_image(&mut b);
    for _ in 0..2 {
        b.page(IMAGE_PAGE, dictionary! { "Im1" => image });
    }
    let input = b.finish(&workspace, "shared-image.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(172.0, 600.0, 100.0, 100.0),
        "out.pdf",
    );

    assert_eq!(report.images_blanked, 1);
    let images = image_streams(&out);
    assert_eq!(images.len(), 2);
    let untouched = images
        .iter()
        .filter(|s| decode_rgb(s) == rgb_samples())
        .count();
    assert_eq!(untouched, 1, "page two keeps the original picture");
}

#[test]
fn an_image_inside_a_scaled_form_is_blanked_where_the_box_lands() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let image = rgb_image(&mut b);
    // The same picture, but drawn through a form scaled by two and moved:
    // 100pt square at the form's origin becomes 200pt at (72, 500).
    let form = b.form(
        "q 100 0 0 100 0 0 cm /Im1 Do Q",
        [2, 0, 0, 2, 72, 500],
        dictionary! { "Im1" => image },
    );
    b.page("/Fm1 Do", dictionary! { "Fm1" => form });
    let input = b.finish(&workspace, "form-image.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(172.0, 600.0, 100.0, 100.0),
        "out.pdf",
    );

    assert_eq!(report.forms_edited, 1);
    assert_eq!(report.images_blanked, 1);
    let images = image_streams(&out);
    assert_eq!(images.len(), 1);
    let samples = decode_rgb(&images[0]);
    let at = |x: u32, y: u32| {
        let i = ((y * SIDE + x) * 3) as usize;
        [samples[i], samples[i + 1], samples[i + 2]]
    };
    assert_eq!(at(75, 25), [0, 0, 0]);
    assert_eq!(at(25, 75), pixel(25, 75));
    assert_eq!(at(25, 25), pixel(25, 25));
}

#[test]
fn a_box_beside_the_image_leaves_it_alone() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let image = rgb_image(&mut b);
    b.page(IMAGE_PAGE, dictionary! { "Im1" => image });
    let input = b.finish(&workspace, "image.pdf");

    // Touches the picture's right edge at x=272 but covers nothing.
    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(272.0, 600.0, 50.0, 50.0),
        "out.pdf",
    );

    assert_eq!(report.images_blanked, 0);
    let images = image_streams(&out);
    assert_eq!(images.len(), 1);
    assert_eq!(decode_rgb(&images[0]), rgb_samples());
}

#[test]
fn a_jpeg_is_blanked_and_stays_a_jpeg() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let jpeg = common::jpeg_bytes(SIDE, SIDE, 95);
    let original = image::load_from_memory(&jpeg).unwrap().to_rgb8();
    let image = b.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => SIDE as i64,
            "Height" => SIDE as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg,
    ));
    b.page(IMAGE_PAGE, dictionary! { "Im1" => image });
    let input = b.finish(&workspace, "jpeg.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(172.0, 600.0, 100.0, 100.0),
        "out.pdf",
    );

    assert_eq!(report.images_blanked, 1);
    let images = image_streams(&out);
    assert_eq!(images.len(), 1);
    assert_eq!(
        images[0]
            .dict
            .get(b"Filter")
            .and_then(Object::as_name)
            .unwrap(),
        b"DCTDecode"
    );
    let edited = image::load_from_memory(&images[0].content)
        .unwrap()
        .to_rgb8();

    // Inside the covered quarter, away from the 8-pixel blocks on its edge.
    for y in 0..48 {
        for x in 56..100 {
            let p = edited.get_pixel(x, y).0;
            assert!(p.iter().all(|c| *c < 12), "({x}, {y}) is {p:?}");
        }
    }
    // Away from the edge, the rest is the picture to within JPEG noise.
    let (mut total, mut count) = (0u64, 0u64);
    for y in 60..100 {
        for x in 0..40 {
            let (a, b) = (edited.get_pixel(x, y).0, original.get_pixel(x, y).0);
            total += (0..3).map(|i| u64::from(a[i].abs_diff(b[i]))).sum::<u64>();
            count += 3;
        }
    }
    assert!(total / count < 8, "mean difference {}", total / count);
}

#[test]
fn an_indexed_image_with_four_bit_samples_is_blanked() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    // 8 by 8, 4 bits each, all index 1 (a bright red).
    let samples = vec![0x11u8; 4 * 8];
    let image = b.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 8,
            "Height" => 8,
            "BitsPerComponent" => 4,
            "ColorSpace" => vec![
                "Indexed".into(),
                "DeviceRGB".into(),
                1.into(),
                Object::string_literal(vec![0, 0, 0, 255, 0, 0]),
            ],
            "Filter" => "FlateDecode",
        },
        deflate(&samples),
    ));
    // 80pt square at (100, 500): one pixel is ten points.
    b.page(
        "q 80 0 0 80 100 500 cm /Im1 Do Q",
        dictionary! { "Im1" => image },
    );
    let input = b.finish(&workspace, "indexed.pdf");

    // Covers the lower left quarter: pixels x 0..4, rows 4..8.
    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(90.0, 490.0, 50.0, 50.0),
        "out.pdf",
    );

    assert_eq!(report.images_blanked, 1);
    let images = image_streams(&out);
    assert_eq!(images.len(), 1);
    let samples = decode_rgb(&images[0]);
    assert_eq!(samples.len(), 8 * 8 * 3);
    let at = |x: usize, y: usize| {
        let i = (y * 8 + x) * 3;
        [samples[i], samples[i + 1], samples[i + 2]]
    };
    assert_eq!(at(1, 6), [0, 0, 0]);
    assert_eq!(at(6, 1), [255, 0, 0]);
    assert_eq!(at(6, 6), [255, 0, 0]);
}

#[test]
fn a_stencil_mask_has_its_covered_bits_set_to_unpainted() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    // 16 by 16, every bit 0: every pixel painted.
    let image = b.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 16,
            "Height" => 16,
            "BitsPerComponent" => 1,
            "ImageMask" => true,
            "Filter" => "FlateDecode",
        },
        deflate(&[0u8; 32]),
    ));
    // 160pt square at (100, 500): ten points a pixel.
    b.page(
        "q 160 0 0 160 100 500 cm /Im1 Do Q",
        dictionary! { "Im1" => image },
    );
    let input = b.finish(&workspace, "stencil.pdf");

    // The top left pixel block: x 0..4, rows 0..4.
    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(95.0, 625.0, 40.0, 40.0),
        "out.pdf",
    );

    assert_eq!(report.images_blanked, 1);
    let images = image_streams(&out);
    let bits = inflate(&images[0].content);
    assert_eq!(bits.len(), 32);
    // Row 0, first byte: pixels 0..8, of which 0..4 are covered.
    assert_eq!(bits[0], 0b1111_0000);
    assert_eq!(bits[2 * 3], 0b1111_0000);
    assert_eq!(bits[2 * 4], 0);
}

#[test]
fn an_image_that_cannot_be_decoded_is_removed_rather_than_left() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let image = b.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 8,
            "Height" => 8,
            "BitsPerComponent" => 1,
            "ColorSpace" => "DeviceGray",
            "Filter" => "CCITTFaxDecode",
        },
        b"SCANNED-SECRET".to_vec(),
    ));
    b.page(IMAGE_PAGE, dictionary! { "Im1" => image });
    let input = b.finish(&workspace, "ccitt.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "1",
        rect(172.0, 600.0, 100.0, 100.0),
        "out.pdf",
    );

    assert_eq!(report.images_removed, 1);
    assert_eq!(report.images_blanked, 0);
    assert!(image_streams(&out).is_empty());
    assert!(!has_bytes(&out, "SCANNED-SECRET"));
    assert!(!page_operations(&out, 1)
        .iter()
        .any(|op| op.operator == "Do"));
}

// ------------------------------------------------------------------- misc

#[test]
fn a_page_thumbnail_is_dropped() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    let thumb = rgb_image(&mut b);
    b.page(IMAGE_PAGE, dictionary! {});
    let page_id = match b.kids[0] {
        Object::Reference(id) => id,
        _ => unreachable!(),
    };
    b.doc
        .get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Thumb", thumb);
    let input = b.finish(&workspace, "thumb.pdf");

    let (out, _) = redacted(
        &workspace,
        &input,
        "1",
        rect(172.0, 600.0, 100.0, 100.0),
        "out.pdf",
    );
    assert!(image_streams(&out).is_empty());
}

#[test]
fn only_the_selected_pages_are_touched() {
    let workspace = Workspace::new();
    let mut b = Builder::new();
    for _ in 0..2 {
        b.page(
            "BT /F1 12 Tf 72 700 Td (Account 12345) Tj ET",
            dictionary! {},
        );
    }
    let input = b.finish(&workspace, "two.pdf");

    let (out, report) = redacted(
        &workspace,
        &input,
        "2",
        rect(60.0, 690.0, 200.0, 30.0),
        "out.pdf",
    );

    assert_eq!(report.pages_redacted, 1);
    if let Some(pages) = pages_text(&out) {
        assert!(pages[0].contains("12345"), "{:?}", pages[0]);
        assert!(!pages[1].contains("12345"), "{:?}", pages[1]);
    }
}
