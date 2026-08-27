//! Synthetic documents for the integration tests.
//!
//! Fixtures are generated rather than committed as binaries. Page geometry and
//! attribute placement are exactly the things these tests need to vary, and a
//! handful of checked-in files cannot cover the combinations — in particular
//! whether `/MediaBox` and `/Resources` sit on the page or are inherited from
//! the page tree, which is where reassembly bugs hide.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use tempfile::TempDir;

/// Where a page's inheritable attributes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attributes {
    /// `/MediaBox` and `/Resources` on each page. The easy case.
    OnPage,
    /// Both on the `/Pages` node, inherited by every page. The case that breaks
    /// naive merges.
    Inherited,
}

/// A4 in PDF points.
pub const A4: (i64, i64) = (595, 842);
/// US Letter in PDF points, so that differently-sized inputs can be told apart.
pub const LETTER: (i64, i64) = (612, 792);

/// Build a document whose pages are labelled `{label} 1`, `{label} 2`, and so
/// on, so that page identity survives into the content stream and can be
/// asserted on after a reassembly.
pub fn build(page_count: usize, label: &str, size: (i64, i64), attributes: Attributes) -> Document {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let media_box: Vec<Object> = vec![0.into(), 0.into(), size.0.into(), size.1.into()];

    let mut kids = Vec::with_capacity(page_count);
    for number in 1..=page_count {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 36.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal(format!("{label} {number}"))],
                ),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));

        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        };

        if attributes == Attributes::OnPage {
            page.set("Resources", Object::Reference(resources_id));
            page.set("MediaBox", Object::Array(media_box.clone()));
        }

        kids.push(Object::Reference(doc.add_object(page)));
    }

    let mut pages = dictionary! {
        "Type" => "Pages",
        "Count" => page_count as i64,
        "Kids" => Object::Array(kids),
    };

    if attributes == Attributes::Inherited {
        pages.set("Resources", Object::Reference(resources_id));
        pages.set("MediaBox", Object::Array(media_box));
    }

    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    doc
}

/// A scratch directory that cleans itself up, plus helpers for putting
/// documents in it.
pub struct Workspace {
    dir: TempDir,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new().expect("could not create a temporary directory"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Path inside the workspace, without creating anything.
    pub fn join(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Write a generated document into the workspace and return its path.
    pub fn write(&self, name: &str, mut doc: Document) -> PathBuf {
        let path = self.join(name);
        doc.save(&path).expect("could not write fixture");
        path
    }

    /// Shorthand for the common case: an N-page A4 document with per-page
    /// attributes.
    pub fn document(&self, name: &str, page_count: usize, label: &str) -> PathBuf {
        self.write(name, build(page_count, label, A4, Attributes::OnPage))
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

/// Read back the text each page draws, in page order.
///
/// This is how the tests assert that pages kept their identity and order
/// through an operation.
pub fn page_labels(path: &Path) -> Vec<String> {
    let doc = Document::load(path).expect("could not reload document");

    doc.get_pages()
        .into_values()
        .map(|page_id| {
            let content = doc
                .get_page_content(page_id)
                .expect("page has no content stream");
            let decoded = Content::decode(&content).expect("could not decode content stream");

            decoded
                .operations
                .iter()
                .filter(|operation| operation.operator == "Tj")
                .filter_map(|operation| operation.operands.first())
                .filter_map(|operand| match operand {
                    Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Read each page's effective `/MediaBox`, following inheritance.
pub fn page_media_boxes(path: &Path) -> Vec<Vec<i64>> {
    let doc = Document::load(path).expect("could not reload document");

    doc.get_pages()
        .into_values()
        .map(|page_id| {
            let mut current = Some(page_id);
            let mut depth = 0;

            while let Some(id) = current {
                depth += 1;
                if depth > 32 {
                    break;
                }

                let Ok(dict) = doc.get_dictionary(id) else {
                    break;
                };

                if let Ok(Object::Array(values)) = dict.get(b"MediaBox") {
                    return values
                        .iter()
                        .map(|value| value.as_i64().unwrap_or_default())
                        .collect();
                }

                current = dict
                    .get(b"Parent")
                    .ok()
                    .and_then(|object| object.as_reference().ok());
            }

            Vec::new()
        })
        .collect()
}

/// Every distinct object id referenced from the page tree's `/Kids`.
pub fn page_ids(path: &Path) -> Vec<(u32, u16)> {
    let doc = Document::load(path).expect("could not reload document");
    doc.get_pages().into_values().collect()
}
