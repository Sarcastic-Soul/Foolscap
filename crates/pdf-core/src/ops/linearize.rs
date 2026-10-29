//! Linearization: rewriting a PDF for fast web view (byte-serving).
//!
//! A linearized PDF is laid out so a reader that has only the first chunk of
//! the file can already show page 1: the file carries a linearization
//! dictionary as its first object, then the catalog, the page-tree root, page 1
//! and everything page 1 needs, then a small cross-reference section and
//! trailer of its own, and only then the rest of the document followed by the
//! main cross-reference whose `/Prev` chains back to the first section.
//!
//! `lopdf`'s `SaveOptions { linearize: true }` looks like it does this, but in
//! 0.44 the writer never reads the flag — it saves a perfectly ordinary file.
//! The serializer here therefore does the work by hand: objects are renumbered
//! into the two-section order, and the linearization dictionary's offsets are
//! spliced in once the whole image has been rendered, so every value describes
//! the file that is actually written.
//!
//! No hint stream is written (`/H [0 0]`): hints are an optimisation for
//! fetching *later* pages over range requests, they are optional, and Poppler
//! rejects more hint layouts than it accepts. Page one still byte-serves from
//! the head of the file without them.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use lopdf::{Dictionary, Object, ObjectId, Stream};

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::progress::{Progress, ProgressFn};

/// Catalog keys that index pages by position. Following them while collecting
/// the first-page section would drag every page into it — outlines and name
/// trees point at pages all over the document — so they stay in the remainder
/// section, the same trade-off `assemble` makes when it rebuilds a page tree.
const DEFERRED_CATALOG_KEYS: [&[u8]; 3] = [b"Outlines", b"Names", b"StructTreeRoot"];

/// Byte width of every offset-bearing entry in the linearization dictionary.
/// Fixed width is what lets the dictionary be written with placeholders and
/// spliced afterwards without shifting anything that follows it.
const NUMBER_WIDTH: usize = 10;

/// Linearize a PDF document for fast web view (byte-serving).
///
/// The document is replaced by its linearized image: [`is_linearized`] reports
/// `true` and the objects are renumbered into first-page-first order. Writing
/// that document back out with [`Document::save`] does **not** preserve the
/// two-section layout — the plain writer always emits a single cross-reference
/// section — so callers that want a linearized *file* should use
/// [`write_linearized`] instead.
pub fn linearize(doc: &mut Document) -> Result<()> {
    let progress_fn: ProgressFn = &mut |_| {};
    linearize_with_progress(doc, progress_fn)
}

/// Linearize a PDF document with progress reporting.
pub fn linearize_with_progress(doc: &mut Document, progress: ProgressFn) -> Result<()> {
    progress(Progress {
        current: 0,
        total: Some(1),
        message: "Linearizing document".into(),
    });

    let bytes = linearized_bytes(&doc.inner)?;
    doc.inner = lopdf::Document::load_mem(&bytes).map_err(|source| {
        PdfError::Internal(format!(
            "linearized image failed to reload, so it would not have been a valid PDF: {source}"
        ))
    })?;

    progress(Progress {
        current: 1,
        total: Some(1),
        message: "Linearization complete".into(),
    });

    tracing::debug!("document linearized for fast web view");
    Ok(())
}

/// Serialize `doc` as a linearized PDF and write it to `path`.
///
/// Unlike [`Document::save`], the file on disk keeps the two-section layout:
/// the linearization dictionary, the catalog, the page-tree root, page 1 and
/// its dependencies, and a first-page cross-reference and trailer all sit near
/// the front, so a reader can display page 1 from the first chunk of a
/// byte-served download. Already-linearized input is fine — any stale
/// linearization dictionary is dropped and rebuilt.
pub fn write_linearized(doc: &Document, path: &Path) -> Result<()> {
    let bytes = linearized_bytes(&doc.inner)?;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|source| PdfError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    std::fs::write(path, bytes).map_err(|source| PdfError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Check if a document is already linearized.
pub fn is_linearized(doc: &Document) -> bool {
    // In a linearized PDF, there is a linearization dictionary with a
    // `/Linearized` entry among the objects.
    doc.inner
        .objects
        .values()
        .any(|obj| matches!(obj, Object::Dictionary(dict) if dict.has(b"Linearized")))
}

/// Where each object goes in the linearized image.
struct Layout {
    /// Section one in write order: the catalog, the page-tree root, page 1
    /// immediately after them, then the rest of the first page's dependencies.
    first: Vec<ObjectId>,
    /// Everything else: pages two onwards, each page's own objects in document
    /// order, then whatever no page claimed.
    rest: Vec<ObjectId>,
    /// Original object id to the id it takes in the image.
    renumbered: HashMap<ObjectId, ObjectId>,
    /// The id the first page takes in the image, for `/O`.
    first_page: u32,
    /// Page count, for `/N`.
    pages: usize,
    /// The catalog's id in the image, for both trailers' `/Root`.
    root: u32,
}

impl Layout {
    /// The highest object number in the image: the linearization dictionary,
    /// section one, and the remainder.
    fn last_object(&self) -> u32 {
        self.first.len() as u32 + self.rest.len() as u32 + 1
    }
}

/// Decide the two-section order for every object in the document.
fn plan_layout(doc: &lopdf::Document) -> Result<Layout> {
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    if pages.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    let root_id = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|object| object.as_reference().ok())
        .ok_or_else(|| PdfError::Internal("the trailer has no /Root reference".into()))?;
    let catalog = doc
        .get_dictionary(root_id)
        .map_err(|source| PdfError::Internal(format!("the catalog is unreadable: {source}")))?;
    let tree_id = catalog
        .get(b"Pages")
        .ok()
        .and_then(|object| object.as_reference().ok())
        .ok_or_else(|| PdfError::Internal("the catalog has no /Pages reference".into()))?;

    // The intermediate nodes between the tree root and page one. Only these
    // survive the /Kids cut when the first-page section is collected.
    let mut ancestors: HashSet<ObjectId> = HashSet::new();
    let mut current = pages[0];
    for _ in 0..64 {
        let Ok(dict) = doc.get_dictionary(current) else {
            break;
        };
        let Some(parent) = dict
            .get(b"Parent")
            .ok()
            .and_then(|object| object.as_reference().ok())
        else {
            break;
        };
        if !ancestors.insert(parent) {
            break;
        }
        current = parent;
    }

    // Everything page one and the document skeleton need up front. A stale
    // /Linearized dictionary is dropped here, and object streams with it:
    // their contents were expanded into ordinary objects when the file was
    // loaded, so writing both would duplicate every contained object.
    let mut wanted: HashSet<ObjectId> = ancestors;
    wanted.insert(pages[0]);
    let first_set = closure(doc, &[root_id, tree_id, pages[0]], &wanted);

    let mut first: Vec<ObjectId> = vec![root_id, tree_id, pages[0]];
    let mut taken: HashSet<ObjectId> = first.iter().copied().collect();
    for id in first_set {
        if is_real(doc, id) && taken.insert(id) {
            first.push(id);
        }
    }

    // Each later page claims its own objects — the page itself first, then
    // its contents and resources — minus whatever section one already took.
    // Tree nodes and resources no single page claimed end up in the tail.
    let mut rest: Vec<ObjectId> = Vec::new();
    for page in &pages[1..] {
        let empty = HashSet::new();
        let run = closure(doc, std::slice::from_ref(page), &empty);
        if taken.insert(*page) {
            rest.push(*page);
        }
        for id in run {
            if id != *page && is_real(doc, id) && taken.insert(id) {
                rest.push(id);
            }
        }
    }

    // Anything left: intermediate page-tree nodes, outlines, name trees,
    // shared fonts, the information dictionary, and so on.
    for (id, object) in &doc.objects {
        if taken.insert(*id) && is_real_object(object) {
            rest.push(*id);
        }
    }

    // Number the image: 1 is the linearization dictionary, then section one,
    // then the remainder in order.
    let mut renumbered: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut next = 1u32;
    for id in first.iter().chain(rest.iter()) {
        next += 1;
        renumbered.insert(*id, (next, 0));
    }

    let first_page = renumbered[&pages[0]].0;
    let root = renumbered[&root_id].0;
    Ok(Layout {
        first,
        rest,
        renumbered,
        first_page,
        pages: pages.len(),
        root,
    })
}

/// The objects reachable from `roots`, in document order, under the cuts that
/// keep the first-page section small: later pages are not entered, `/Kids` is
/// followed only towards page one, and a page's `/Parent` is not walked back
/// up. `wanted` lists the nodes those cuts let through.
fn closure(doc: &lopdf::Document, roots: &[ObjectId], wanted: &HashSet<ObjectId>) -> Vec<ObjectId> {
    let mut found: Vec<ObjectId> = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut queue: Vec<ObjectId> = roots.to_vec();

    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Ok(object) = doc.get_object(id) else {
            continue;
        };
        found.push(id);

        // References worth following, with the first-page cuts applied.
        let mut follow: Vec<ObjectId> = Vec::new();
        match object {
            Object::Dictionary(dict) => {
                collect_from_dictionary(doc, dict, roots, wanted, &mut follow)
            }
            Object::Stream(stream) => {
                collect_from_dictionary(doc, &stream.dict, roots, wanted, &mut follow)
            }
            Object::Array(items) => {
                for id in items.iter().flat_map(references) {
                    follow.push(id);
                }
            }
            _ => {}
        }
        queue.extend(follow);
    }

    found.sort_unstable();
    found.dedup();
    found
}

/// The outgoing references of a dictionary under the collection cuts.
fn collect_from_dictionary(
    doc: &lopdf::Document,
    dict: &Dictionary,
    roots: &[ObjectId],
    wanted: &HashSet<ObjectId>,
    follow: &mut Vec<ObjectId>,
) {
    let type_of = type_name_of;
    let is_catalog = type_of(dict) == b"Catalog".as_slice();
    let is_tree = type_of(dict) == b"Pages".as_slice();
    let is_page = type_of(dict) == b"Page".as_slice();

    for (key, value) in dict.iter() {
        // Outlines and name trees point at pages all over the document; they
        // belong to the remainder, not the first-page section.
        if is_catalog && DEFERRED_CATALOG_KEYS.contains(&key.as_slice()) {
            continue;
        }
        // Walking a page's /Parent pulls shared tree nodes into a per-page run.
        if is_page && key.as_slice() == b"Parent" {
            continue;
        }
        for target in references(value) {
            let target_type = doc
                .get_dictionary(target)
                .ok()
                .and_then(|dict| dict.get(b"Type").ok())
                .and_then(|object| object.as_name().ok())
                .unwrap_or(b"".as_slice());

            // Kids: through to page one and the nodes on the path to it only.
            // Every other kid is a later page or a subtree holding one.
            if is_tree && key.as_slice() == b"Kids" {
                if wanted.contains(&target) {
                    follow.push(target);
                }
                continue;
            }
            // Never enter a later page while collecting.
            if target_type == b"Page".as_slice()
                && !wanted.contains(&target)
                && !roots.contains(&target)
            {
                continue;
            }
            follow.push(target);
        }
    }
}

/// Whether an id still refers to an object the image should carry: not an
/// object stream or cross-reference stream (their contents were expanded on
/// load) and not a stale linearization dictionary.
fn is_real(doc: &lopdf::Document, id: ObjectId) -> bool {
    doc.get_object(id).ok().map(is_real_object).unwrap_or(false)
}

fn is_real_object(object: &Object) -> bool {
    match object {
        Object::Dictionary(dict) => {
            !dict.has(b"Linearized") && !has_type(dict, b"ObjStm") && !has_type(dict, b"XRef")
        }
        Object::Stream(stream) => {
            !has_type(&stream.dict, b"ObjStm") && !has_type(&stream.dict, b"XRef")
        }
        _ => true,
    }
}

/// The dictionary's `/Type`, when it carries a name there.
fn has_type(dict: &Dictionary, name: &[u8]) -> bool {
    dict.get(b"Type").and_then(Object::as_name).ok() == Some(name)
}

/// A dictionary's `/Type` as a slice, empty when absent or not a name.
fn type_name_of(dict: &Dictionary) -> &[u8] {
    dict.get(b"Type")
        .and_then(Object::as_name)
        .ok()
        .unwrap_or(b"".as_slice())
}

/// Every object id referenced from `value`, at any depth.
fn references(value: &Object) -> Vec<ObjectId> {
    match value {
        Object::Reference(id) => vec![*id],
        Object::Array(items) => items.iter().flat_map(references).collect(),
        Object::Dictionary(dict) => dict
            .iter()
            .flat_map(|(_, value)| references(value))
            .collect(),
        Object::Stream(stream) => references(&Object::Dictionary(stream.dict.clone())),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------- writing

/// Render the whole linearized image in one pass. Offset-bearing values in the
/// linearization dictionary are written as fixed-width placeholders and
/// spliced once their positions are known, so nothing is rendered twice.
fn linearized_bytes(doc: &lopdf::Document) -> Result<Vec<u8>> {
    let layout = plan_layout(doc)?;

    // Clone the objects with every reference rewritten to its new id; the
    // originals stay untouched so nothing is half-mutated if a reference turns
    // out to dangle.
    let mut objects: HashMap<u32, Object> = HashMap::new();
    for id in layout.first.iter().chain(layout.rest.iter()) {
        let object = doc
            .get_object(*id)
            .map_err(|source| PdfError::Internal(format!("object {id:?} vanished: {source}")))?;
        let mapped = remap(object, &layout.renumbered)?;
        objects.insert(layout.renumbered[id].0, mapped);
    }

    let mut out = Vec::new();
    let mut positions: HashMap<u32, (usize, usize)> = HashMap::new();

    // Header.
    let version = if doc.version.is_empty() {
        "1.5"
    } else {
        doc.version.as_str()
    };
    out.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

    // The linearization dictionary, object 1, with placeholders to splice.
    // `/H [0 0]` says there is no hint stream: a v1 hold-out, noted below.
    let first_object_offset = out.len();
    out.extend_from_slice(b"1 0 obj\n<< /Linearized 1 /L ");
    let l_slot = reserve_number(&mut out);
    out.extend_from_slice(b" /H [ 0 0 ] /O ");
    out.extend_from_slice(layout.first_page.to_string().as_bytes());
    out.extend_from_slice(b" /E ");
    let e_slot = reserve_number(&mut out);
    out.extend_from_slice(b" /N ");
    out.extend_from_slice(layout.pages.to_string().as_bytes());
    out.extend_from_slice(b" /T ");
    let t_slot = reserve_number(&mut out);
    out.extend_from_slice(b" >>\nendobj\n");
    positions.insert(1, (first_object_offset, out.len() - first_object_offset));

    // First-page section.
    for id in &layout.first {
        let number = layout.renumbered[id].0;
        write_indirect(&mut out, number, &objects[&number], &mut positions)?;
    }
    let end_of_first_section = out.len();

    // First-page cross-reference and trailer: enough for a reader that has
    // only the head of the file to find the catalog and show page one.
    let first_xref_start = out.len();
    let first_count = layout.first.len() + 2; // the free head, object 1, section one
    out.extend_from_slice(format!("xref\n0 {first_count}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..first_count as u32 {
        let (offset, _) = positions[&number];
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {} 0 R >>\nstartxref\n{first_xref_start}\n%%EOF\n",
            layout.last_object() + 2,
            layout.root
        )
        .as_bytes(),
    );

    // Remainder: pages two onwards in their runs, then the shared tail.
    for id in &layout.rest {
        let number = layout.renumbered[id].0;
        write_indirect(&mut out, number, &objects[&number], &mut positions)?;
    }

    // Main cross-reference and trailer, chaining back to the first section.
    // The main table is complete — every object in the file, from zero —
    // because readers addressing entries by position (Poppler computes a
    // missing entry's bytes at /T plus 20 times its number) expect it dense.
    let main_xref_start = out.len();
    let total_count = layout.last_object() + 1;
    out.extend_from_slice(format!("xref\n0 {total_count}\n").as_bytes());
    let t_value = out.len(); // /T: the offset of the object-0 entry row
    out.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..total_count {
        let (offset, _) = positions[&number];
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {} 0 R",
            total_count, layout.root
        )
        .as_bytes(),
    );
    if let Ok(info) = doc.trailer.get(b"Info") {
        let mapped = remap(info, &layout.renumbered)?;
        out.extend_from_slice(b" /Info ");
        write_object(&mut out, &mapped)?;
    }
    if let Ok(id) = doc.trailer.get(b"ID") {
        out.extend_from_slice(b" /ID ");
        write_object(&mut out, id)?;
    }
    out.extend_from_slice(
        format!(" /Prev {first_xref_start} >>\nstartxref\n{main_xref_start}\n%%EOF").as_bytes(),
    );

    // Splice the offset-bearing values now that every position is known.
    let file_length = out.len();
    splice(&mut out, l_slot, file_length);
    splice(&mut out, e_slot, end_of_first_section);
    splice(&mut out, t_slot, t_value);

    Ok(out)
}

/// Write `NUMBER_WIDTH` placeholder digits and return where the real number
/// will be spliced in.
fn reserve_number(out: &mut Vec<u8>) -> usize {
    let slot = out.len();
    out.extend(std::iter::repeat_n(b'0', NUMBER_WIDTH));
    slot
}

/// Overwrite a reserved number slot with the value, at the same width.
fn splice(out: &mut [u8], slot: usize, value: usize) {
    let text = format!("{value:0width$}", width = NUMBER_WIDTH);
    out[slot..slot + NUMBER_WIDTH].copy_from_slice(text.as_bytes());
}

/// Write one indirect object, recording where it landed.
fn write_indirect(
    out: &mut Vec<u8>,
    number: u32,
    object: &Object,
    positions: &mut HashMap<u32, (usize, usize)>,
) -> Result<()> {
    let start = out.len();
    out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    write_object(out, object)?;
    out.extend_from_slice(b"\nendobj\n");
    positions.insert(number, (start, out.len() - start));
    Ok(())
}

/// Serialize one PDF object in conventional syntax. Strings are written as
/// hexadecimal, which needs no escaping and is valid everywhere.
fn write_object(out: &mut Vec<u8>, object: &Object) -> Result<()> {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(value) => out.extend_from_slice(if *value { b"true" } else { b"false" }),
        Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Real(value) => out.extend_from_slice(format_real(*value).as_bytes()),
        Object::Name(name) => write_name(out, name),
        Object::String(bytes, _) => {
            out.extend_from_slice(b"<");
            for byte in bytes {
                out.extend_from_slice(format!("{byte:02X}").as_bytes());
            }
            out.extend_from_slice(b">");
        }
        Object::Array(items) => {
            out.extend_from_slice(b"[ ");
            for item in items {
                write_object(out, item)?;
                out.extend_from_slice(b" ");
            }
            out.extend_from_slice(b"]");
        }
        Object::Dictionary(dict) => write_dictionary(out, dict, None)?,
        Object::Stream(stream) => {
            write_dictionary(out, &stream.dict, Some(stream.content.len()))?;
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&stream.content);
            out.extend_from_slice(b"\nendstream");
        }
        Object::Reference(id) => out.extend_from_slice(format!("{} {} R", id.0, id.1).as_bytes()),
    }
    Ok(())
}

/// Serialize a dictionary, with `/Length` forced for a stream when given.
fn write_dictionary(out: &mut Vec<u8>, dict: &Dictionary, length: Option<usize>) -> Result<()> {
    out.extend_from_slice(b"<< ");
    for (key, value) in dict.iter() {
        write_name(out, key);
        out.extend_from_slice(b" ");
        if key.as_slice() == b"Length" {
            if let Some(length) = length {
                // Never trust a stale length from the source file.
                out.extend_from_slice(length.to_string().as_bytes());
                out.extend_from_slice(b" ");
                continue;
            }
            write_object(out, value)?;
        } else {
            write_object(out, value)?;
        }
        out.extend_from_slice(b" ");
    }
    if let Some(length) = length {
        if dict.get(b"Length").is_err() {
            out.extend_from_slice(b"/Length ");
            out.extend_from_slice(length.to_string().as_bytes());
            out.extend_from_slice(b" ");
        }
    }
    out.extend_from_slice(b">>");
    Ok(())
}

/// A name with every byte that could confuse a tokenizer escaped as `#xx`.
fn write_name(out: &mut Vec<u8>, name: &[u8]) {
    out.push(b'/');
    for byte in name {
        let regular = byte.is_ascii_alphanumeric()
            || (b'!'..=b'~').contains(byte)
                && !matches!(
                    byte,
                    b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
                );
        if regular {
            out.push(*byte);
        } else {
            out.extend_from_slice(format!("#{byte:02X}").as_bytes());
        }
    }
}

/// Reals in a form PDF allows: no exponent, and a non-finite value (which
/// should never occur) degrades to zero rather than invalid syntax.
fn format_real(value: f32) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "0".to_string()
    }
}

/// Rewrite every reference through the renumbering table. A reference to an
/// object that is not in the table means the source document is inconsistent
/// in a way this serializer refuses to paper over.
fn remap(object: &Object, renumbered: &HashMap<ObjectId, ObjectId>) -> Result<Object> {
    match object {
        Object::Reference(id) => Ok(Object::Reference(renumbered.get(id).copied().ok_or_else(
            || PdfError::Internal(format!("object {id:?} is referenced but absent")),
        )?)),
        Object::Array(items) => Ok(Object::Array(
            items
                .iter()
                .map(|item| remap(item, renumbered))
                .collect::<Result<Vec<_>>>()?,
        )),
        Object::Dictionary(dict) => {
            let mut mapped = Dictionary::new();
            for (key, value) in dict.iter() {
                mapped.set(key.clone(), remap(value, renumbered)?);
            }
            Ok(Object::Dictionary(mapped))
        }
        Object::Stream(stream) => {
            let dict = match remap(&Object::Dictionary(stream.dict.clone()), renumbered)? {
                Object::Dictionary(dict) => dict,
                _ => unreachable!("a dictionary always remaps to a dictionary"),
            };
            Ok(Object::Stream(Stream {
                dict,
                content: stream.content.clone(),
                allows_compression: stream.allows_compression,
                start_position: None,
            }))
        }
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::crop::PageSize;

    /// A multi-page document whose pages each have content, so the runs the
    /// hint table describes are non-trivial.
    fn multi_page(pages: usize) -> Document {
        crate::ops::create::create_blank(pages, PageSize::A4).unwrap()
    }

    fn dictionary_value(bytes: &[u8], key: &[u8]) -> Option<u64> {
        let marker = [b"/", key, b" "].concat();
        let start = bytes.windows(marker.len()).position(|w| w == marker)? + marker.len();
        let digits: Vec<u8> = bytes[start..]
            .iter()
            .copied()
            .take_while(|byte| byte.is_ascii_digit())
            .collect();
        String::from_utf8(digits).ok()?.parse().ok()
    }

    #[test]
    fn a_multi_page_document_linearizes_with_the_dictionary_up_front() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("linearized.pdf");

        let doc = multi_page(3);
        write_linearized(&doc, &path).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        // The linearization dictionary is the first object in the file.
        let position = bytes
            .windows(b"/Linearized".len())
            .position(|window| window == b"/Linearized")
            .expect("a /Linearized dictionary");
        assert!(
            position < 1024,
            "dictionary at byte {position}, not up front"
        );

        // It describes the file that was actually written.
        assert_eq!(
            dictionary_value(&bytes, b"L").unwrap() as usize,
            bytes.len(),
            "/L must be the file length"
        );
        assert_eq!(
            dictionary_value(&bytes, b"N").unwrap(),
            3,
            "/N is page count"
        );
        let e = dictionary_value(&bytes, b"E").unwrap() as usize;
        assert!(
            bytes[..e].ends_with(b"endobj\n"),
            "/E is the end of the first-page section"
        );
        let t = dictionary_value(&bytes, b"T").unwrap() as usize;
        assert!(
            bytes[t..].starts_with(b"0000000000 65535 f"),
            "/T is the offset of the first xref entry row"
        );

        // The reload agrees, and the pages survived.
        let reloaded = Document::open(&path).unwrap();
        assert_eq!(reloaded.page_count(), 3);
        assert!(is_linearized(&reloaded));
    }

    #[test]
    fn the_first_page_and_its_cross_reference_precede_the_remainder() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("front-loaded.pdf");

        let doc = multi_page(4);
        write_linearized(&doc, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        // /O names the first page's object, and that object sits up front,
        // before the second cross-reference section.
        let first_page = dictionary_value(&bytes, b"O").unwrap();
        let marker = format!("{first_page} 0 obj").into_bytes();
        let first_page_at = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("/O must name a real object");
        let second_xref = bytes
            .windows(4)
            .rposition(|window| window == b"xref")
            .expect("a main cross-reference");
        assert!(
            first_page_at < second_xref,
            "page one belongs to the head of the file"
        );

        // The main trailer chains back to the first section.
        let prev = bytes
            .windows(b"/Prev ".len())
            .rposition(|window| window == b"/Prev ")
            .expect("a /Prev entry in the main trailer");
        let first_xref = bytes
            .windows(4)
            .position(|window| window == b"xref")
            .expect("a first cross-reference");
        let start = prev + b"/Prev ".len();
        let digits: String = bytes[start..]
            .iter()
            .map(|byte| *byte as char)
            .take_while(|byte| byte.is_ascii_digit())
            .collect();
        assert_eq!(
            digits.parse::<usize>().unwrap(),
            first_xref,
            "/Prev must point at the first cross-reference section"
        );
    }

    #[test]
    fn already_linearized_input_relinearizes() {
        let workspace = tempfile::tempdir().unwrap();
        let first = workspace.path().join("once.pdf");
        let second = workspace.path().join("twice.pdf");

        let mut doc = multi_page(2);
        linearize(&mut doc).unwrap();
        assert!(is_linearized(&doc));
        write_linearized(&doc, &first).unwrap();

        let mut again = Document::open(&first).unwrap();
        linearize(&mut again).unwrap();
        write_linearized(&again, &second).unwrap();

        let bytes = std::fs::read(&second).unwrap();
        assert_eq!(
            dictionary_value(&bytes, b"L").unwrap() as usize,
            bytes.len()
        );
        assert_eq!(dictionary_value(&bytes, b"N").unwrap(), 2);
        let reloaded = Document::open(&second).unwrap();
        assert_eq!(reloaded.page_count(), 2);
        assert!(is_linearized(&reloaded));
    }

    #[test]
    fn a_plain_save_is_not_linearized() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("plain.pdf");

        let mut doc = multi_page(2);
        doc.save(&path).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(11).any(|window| window == b"/Linearized"));
        let reloaded = Document::open(&path).unwrap();
        assert!(!is_linearized(&reloaded));
    }

    #[test]
    fn a_document_without_pages_is_refused() {
        let mut doc = Document::from_lopdf(lopdf::Document::new(), None);
        let error = linearize(&mut doc).unwrap_err();
        assert!(matches!(error, PdfError::EmptySelection));
    }
}
