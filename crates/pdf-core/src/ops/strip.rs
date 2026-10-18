//! Privacy stripping: remove identifying metadata before sharing.
//!
//! [`Metadata`](crate::document::Metadata) covers the six Info keys Foolscap
//! reads and writes, but producers stash more there — creation dates, tool
//! versions, custom keys — plus an XMP packet in a Metadata stream, and the
//! trailer ID that lets two files be correlated. `strip` removes all of it.
//!
//! Flattening form fields is a separate operation: see [`crate::ops::flatten`].

use lopdf::{Object, StringFormat};

use crate::document::Document;
use crate::error::Result;

/// What [`strip`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StripReport {
    /// Entries cleared from the Info dictionary.
    pub info_keys_removed: usize,
    /// XMP Metadata streams deleted.
    pub xmp_removed: usize,
    /// Whether the trailer ID was rotated.
    pub id_rotated: bool,
}

/// Remove every Info entry, every XMP Metadata stream (and the Catalog
/// reference to it), and rotate the trailer ID.
///
/// Page content and structure are untouched.
pub fn strip(doc: &mut Document) -> Result<StripReport> {
    let mut report = StripReport::default();

    // Every key, not just the six Metadata knows: dates, tool versions,
    // producer custom keys. The dictionary object goes away entirely and the
    // trailer entry with it; Info is optional in the file format.
    if let Ok(info) = doc.inner.trailer.get(b"Info").cloned() {
        if let Ok(id) = info.as_reference() {
            if let Ok(dict) = doc.inner.get_dictionary(id).cloned() {
                report.info_keys_removed = dict.len();
            }
            doc.inner.objects.remove(&id);
        } else if let Ok(dict) = info.as_dict() {
            report.info_keys_removed = dict.len();
        }
        doc.inner.trailer.remove(b"Info");
    }

    // XMP packets: streams tagged /Type /Metadata or /Subtype /XML. Drop the
    // Catalog reference first so nothing dangles, then delete the streams.
    let xmp: Vec<lopdf::ObjectId> = doc
        .inner
        .objects
        .iter()
        .filter_map(|(id, object)| {
            let Object::Stream(stream) = object else {
                return None;
            };
            let is_metadata = stream
                .dict
                .get(b"Type")
                .and_then(Object::as_name)
                .is_ok_and(|name| name == b"Metadata");
            let is_xml = stream
                .dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|name| name == b"XML");
            (is_metadata || is_xml).then_some(*id)
        })
        .collect();
    report.xmp_removed = xmp.len();
    for id in &xmp {
        doc.inner.objects.remove(id);
    }

    if let Ok(root) = doc.inner.trailer.get(b"Root").cloned() {
        if let Ok(root_id) = root.as_reference() {
            if let Ok(catalog) = doc.inner.get_dictionary_mut(root_id) {
                catalog.remove(b"Metadata");
            }
        }
    }
    // A direct (non-reference) Metadata entry on the Catalog, should one ever
    // appear, is covered by the same removal through the resolved dictionary.
    // The reference-removal above handles what producers write.

    // A stable ID lets recipients correlate this file with other exports of
    // the same source. Fresh random halves break that link.
    let first = Object::String(super::encrypt::fresh_id(16), StringFormat::Hexadecimal);
    let second = Object::String(super::encrypt::fresh_id(16), StringFormat::Hexadecimal);
    doc.inner
        .trailer
        .set("ID", Object::Array(vec![first, second]));
    report.id_rotated = true;

    Ok(report)
}
