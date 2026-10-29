//! Embedded file attachments: list, add, and extract.
//!
//! Attachments live in the catalog's `/Names /EmbeddedFiles` name tree. Each
//! leaf pairs a name with a file specification, whose `/EF /F` points at the
//! file's stream. Reading accepts any tree shape; writing flattens the tree
//! into one sorted list, which every reader understands.

use lopdf::{dictionary, Dictionary, Object, ObjectId, Stream, StringFormat};

use crate::document::Document;
use crate::error::{PdfError, Result};

/// How deep to follow `/Kids` in the name tree before giving up.
const MAX_DEPTH: usize = 32;

/// One embedded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    /// The uncompressed size in bytes.
    pub size: usize,
}

/// List every attachment in the document, sorted by name as the tree stores them.
pub fn list_attachments(doc: &Document) -> Result<Vec<Attachment>> {
    let inner = &doc.inner;
    let mut attachments = Vec::new();
    for (name, spec) in entries(inner) {
        let size = file_bytes(inner, &spec).map_or(0, |bytes| bytes.len());
        attachments.push(Attachment { name, size });
    }
    Ok(attachments)
}

/// The bytes of the attachment called `name`.
pub fn extract_attachment(doc: &Document, name: &str) -> Result<Vec<u8>> {
    entries(&doc.inner)
        .into_iter()
        .find(|(entry, _)| entry == name)
        .and_then(|(_, spec)| file_bytes(&doc.inner, &spec))
        .ok_or_else(|| PdfError::NoSuchAttachment(name.to_string()))
}

/// Embed `data` under `name`, replacing any attachment already called that.
pub fn add_attachment(doc: &mut Document, name: &str, data: &[u8]) -> Result<()> {
    if name.trim().is_empty() {
        return Err(PdfError::InvalidAttachment(
            "the attachment needs a name".into(),
        ));
    }

    let inner = &mut doc.inner;

    let mut stream = Stream::new(
        dictionary! {
            "Type" => "EmbeddedFile",
            "Params" => dictionary! { "Size" => data.len() as i64 },
        },
        data.to_vec(),
    );
    stream.compress()?;
    let stream_id = inner.add_object(stream);

    let text = Object::String(name.as_bytes().to_vec(), StringFormat::Literal);
    let spec = dictionary! {
        "Type" => "Filespec",
        "F" => text.clone(),
        "UF" => text,
        "EF" => dictionary! { "F" => stream_id },
    };
    let spec_id = inner.add_object(spec);

    let mut existing = entries(inner);
    existing.retain(|(entry, _)| entry != name);
    existing.push((name.to_string(), Object::Reference(spec_id)));
    existing.sort_by(|a, b| a.0.cmp(&b.0));

    let mut flat = Vec::with_capacity(existing.len() * 2);
    for (entry, spec) in existing {
        flat.push(Object::String(entry.into_bytes(), StringFormat::Literal));
        flat.push(spec);
    }
    let tree = inner.add_object(dictionary! { "Names" => Object::Array(flat) });

    attach_tree(inner, tree)
}

/// Point the catalog's `/Names /EmbeddedFiles` at `tree`.
fn attach_tree(inner: &mut lopdf::Document, tree: ObjectId) -> Result<()> {
    let names_ref = inner
        .catalog()?
        .get(b"Names")
        .ok()
        .and_then(|object| object.as_reference().ok());

    match names_ref {
        Some(id) => {
            inner
                .get_dictionary_mut(id)?
                .set("EmbeddedFiles", Object::Reference(tree));
        }
        None => {
            let catalog = inner.catalog_mut()?;
            let direct = catalog
                .get(b"Names")
                .map(|object| object.as_dict().is_ok())
                .unwrap_or(false);
            if direct {
                catalog
                    .get_mut(b"Names")?
                    .as_dict_mut()?
                    .set("EmbeddedFiles", Object::Reference(tree));
            } else {
                catalog.set(
                    "Names",
                    dictionary! { "EmbeddedFiles" => Object::Reference(tree) },
                );
            }
        }
    }
    Ok(())
}

/// Every (name, file specification) pair in the embedded-files tree.
fn entries(inner: &lopdf::Document) -> Vec<(String, Object)> {
    let Some(root) = embedded_root(inner) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect(inner, root, &mut out, 0);
    out
}

fn embedded_root(inner: &lopdf::Document) -> Option<&Dictionary> {
    let catalog = inner.catalog().ok()?;
    let names = dict_at(inner, catalog.get(b"Names").ok()?)?;
    dict_at(inner, names.get(b"EmbeddedFiles").ok()?)
}

fn collect(
    inner: &lopdf::Document,
    node: &Dictionary,
    out: &mut Vec<(String, Object)>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        tracing::warn!("attachment name tree nests deeper than {MAX_DEPTH}; the rest is ignored");
        return;
    }

    if let Some(names) = node
        .get(b"Names")
        .ok()
        .and_then(|object| inner.dereference(object).ok())
        .and_then(|(_, object)| object.as_array().ok())
    {
        for pair in names.chunks(2) {
            if let [Object::String(bytes, _), value] = pair {
                out.push((String::from_utf8_lossy(bytes).into_owned(), value.clone()));
            }
        }
    }

    if let Some(kids) = node
        .get(b"Kids")
        .ok()
        .and_then(|object| inner.dereference(object).ok())
        .and_then(|(_, object)| object.as_array().ok())
    {
        for kid in kids {
            if let Some(child) = dict_at(inner, kid) {
                collect(inner, child, out, depth + 1);
            }
        }
    }
}

/// The bytes of the file a specification points at.
fn file_bytes(inner: &lopdf::Document, spec: &Object) -> Option<Vec<u8>> {
    let filespec = dict_at(inner, spec)?;
    let embedded = dict_at(inner, filespec.get(b"EF").ok()?)?;
    let (_, object) = inner.dereference(embedded.get(b"F").ok()?).ok()?;
    object.as_stream().ok()?.decompressed_content().ok()
}

/// A dictionary that is either direct or behind a reference.
fn dict_at<'a>(inner: &'a lopdf::Document, object: &'a Object) -> Option<&'a Dictionary> {
    inner.dereference(object).ok()?.1.as_dict().ok()
}
