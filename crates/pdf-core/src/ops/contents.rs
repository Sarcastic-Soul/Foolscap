//! Editing a page's `/Contents` without disturbing what is already there.
//!
//! `/Contents` is a stream, or an array of streams, and either may sit behind
//! an indirect reference. Operations that draw over or under a page all need
//! the same few moves, so they live here once.

use lopdf::content::Content;
use lopdf::{Dictionary, Object, ObjectId, Stream};

use crate::error::Result;

/// The page's content streams as a list of objects, in drawing order.
///
/// A `/Contents` that is a reference to an array is unpacked, so the list is
/// always streams (or references to them) and never a nested array.
fn content_list(doc: &lopdf::Document, page_id: ObjectId) -> Vec<Object> {
    let Ok(page) = doc.get_dictionary(page_id) else {
        return Vec::new();
    };

    match page.get(b"Contents") {
        Ok(Object::Array(items)) => items.clone(),
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Array(items)) => items.clone(),
            _ => vec![Object::Reference(*id)],
        },
        _ => Vec::new(),
    }
}

fn set_content_list(doc: &mut lopdf::Document, page_id: ObjectId, list: Vec<Object>) -> Result<()> {
    let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
    page.set("Contents", Object::Array(list));
    Ok(())
}

/// Add `layer` after the page's existing content, so it draws on top.
pub(crate) fn append_content(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    layer: ObjectId,
) -> Result<()> {
    let mut list = content_list(doc, page_id);
    list.push(Object::Reference(layer));
    set_content_list(doc, page_id, list)
}

/// Add `layer` before the page's existing content, so the page draws over it.
pub(crate) fn prepend_content(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    layer: ObjectId,
) -> Result<()> {
    let mut list = content_list(doc, page_id);
    list.insert(0, Object::Reference(layer));
    set_content_list(doc, page_id, list)
}

/// Bracket the existing content in `q` and `Q`.
///
/// A page whose own content leaves a transformation or clip in force would
/// otherwise carry it into whatever is appended afterwards. The closing side
/// pops as many levels as the content left open, so that what follows starts
/// from the page's initial state however sloppy the original was.
pub(crate) fn guard_content(doc: &mut lopdf::Document, page_id: ObjectId) -> Result<()> {
    let mut list = content_list(doc, page_id);
    if list.is_empty() {
        return Ok(());
    }

    let left_open = unclosed_saves(&doc.get_page_content(page_id));
    let mut closing = b"\nQ".to_vec();
    for _ in 0..left_open {
        closing.extend_from_slice(b"\nQ");
    }
    closing.push(b'\n');

    let open = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let close = doc.add_object(Stream::new(Dictionary::new(), closing));
    list.insert(0, Object::Reference(open));
    list.push(Object::Reference(close));
    set_content_list(doc, page_id, list)
}

/// How many `q` operators in `content` are never matched by a `Q`.
fn unclosed_saves(content: &[u8]) -> usize {
    let Ok(decoded) = Content::decode(content) else {
        return 0;
    };

    let mut depth = 0usize;
    for operation in &decoded.operations {
        match operation.operator.as_str() {
            "q" => depth += 1,
            // A stray Q is ignored by readers, so it does not go negative.
            "Q" => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_content_leaves_nothing_open() {
        assert_eq!(unclosed_saves(b"q 1 0 0 1 0 0 cm Q"), 0);
    }

    #[test]
    fn unclosed_saves_are_counted() {
        assert_eq!(unclosed_saves(b"q q 1 0 0 1 0 0 cm Q q"), 2);
    }

    #[test]
    fn a_stray_restore_is_not_a_debt() {
        assert_eq!(unclosed_saves(b"Q Q q"), 1);
    }

    #[test]
    fn undecodable_content_counts_as_balanced() {
        assert_eq!(unclosed_saves(b"(unterminated"), 0);
    }
}
