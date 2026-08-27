//! The page-assembly primitive shared by merge, split, and extraction.
//!
//! Every operation that produces a document from a selection of pages funnels
//! through [`assemble`]. Doing it once means the awkward parts — attribute
//! inheritance, object renumbering, page-tree rebuilding — are solved in one
//! place rather than three subtly different ones.

use std::collections::{BTreeMap, HashSet};

use lopdf::{Dictionary, Object, ObjectId};

use crate::error::{PdfError, Result};

/// Attributes a page may inherit from an ancestor node in the page tree
/// instead of carrying itself.
///
/// These are the four the specification makes inheritable. They are the reason
/// naive merges lose page sizes: repointing a page at a new parent silently
/// drops whatever it was inheriting from the old one.
const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Catalog entries that index pages by position and therefore cannot survive a
/// reassembly. Keeping them would leave dangling or, worse, silently wrong
/// references.
const POSITION_DEPENDENT_CATALOG_KEYS: [&[u8]; 3] = [b"Outlines", b"StructTreeRoot", b"Names"];

/// Attributes to copy onto one page, gathered from its ancestors.
type InheritedAttributes = Vec<(&'static [u8], Object)>;

/// One input document and the pages wanted from it, as zero-indexed positions
/// in the order they should appear in the output. Repeats are allowed and
/// produce independent copies of the page.
pub(crate) struct Source {
    pub doc: lopdf::Document,
    pub pages: Vec<usize>,
}

impl Source {
    /// Take every page, in document order.
    pub fn all(doc: lopdf::Document) -> Self {
        let pages = (0..doc.get_pages().len()).collect();
        Self { doc, pages }
    }
}

/// Build a new document from pages selected out of the sources.
///
/// Outlines and the structure tree are dropped: both index pages by position,
/// and carrying them across a reassembly produces bookmarks that point at the
/// wrong page or at nothing.
pub(crate) fn assemble(sources: Vec<Source>) -> Result<lopdf::Document> {
    let mut page_order: Vec<ObjectId> = Vec::new();
    let mut page_objects: BTreeMap<ObjectId, Dictionary> = BTreeMap::new();
    let mut other_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();

    let mut catalog: Option<(ObjectId, Dictionary)> = None;
    let mut pages_node: Option<(ObjectId, Dictionary)> = None;
    let mut info_id: Option<ObjectId> = None;

    let mut version = String::from("1.4");
    let mut next_free_id = 1u32;

    for mut source in sources {
        materialize_inherited_attributes(&mut source.doc)?;

        // Shift this document's object ids clear of everything already taken,
        // so that ids from different documents cannot collide.
        source.doc.renumber_objects_with(next_free_id);
        next_free_id = source.doc.max_id + 1;

        if version_is_older(&version, &source.doc.version) {
            version = source.doc.version.clone();
        }

        if info_id.is_none() {
            info_id = source
                .doc
                .trailer
                .get(b"Info")
                .ok()
                .and_then(|object| object.as_reference().ok());
        }

        let page_ids: Vec<ObjectId> = source.doc.get_pages().into_values().collect();
        let total = page_ids.len();

        let mut wanted: HashSet<ObjectId> = HashSet::new();
        for position in &source.pages {
            let id = *page_ids.get(*position).ok_or(PdfError::PageOutOfRange {
                requested: position + 1,
                total,
            })?;
            page_order.push(id);
            wanted.insert(id);
        }

        for (id, object) in std::mem::take(&mut source.doc.objects) {
            match object.type_name().unwrap_or("") {
                "Page" => {
                    if wanted.contains(&id) {
                        page_objects.insert(id, object.as_dict()?.clone());
                    }
                }
                "Catalog" => {
                    if catalog.is_none() {
                        catalog = Some((id, object.as_dict()?.clone()));
                    }
                }
                "Pages" => {
                    if pages_node.is_none() {
                        pages_node = Some((id, object.as_dict()?.clone()));
                    }
                }
                // Both index pages by position; see the note on the function.
                "Outlines" | "Outline" => {}
                _ => {
                    other_objects.insert(id, object);
                }
            }
        }
    }

    if page_order.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    let mut target = lopdf::Document::with_version(version);
    target.objects = other_objects;
    target.max_id = next_free_id.saturating_sub(1);

    let pages_id = match &pages_node {
        Some((id, _)) => *id,
        None => target.new_object_id(),
    };

    // A page object referenced twice in the page tree is not a valid document —
    // /Parent can only point one way — so repeated selections get their own
    // copy. The copy shares content streams and resources by reference, so it
    // costs a dictionary, not a page's worth of bytes.
    let mut emitted: HashSet<ObjectId> = HashSet::new();
    let mut kids: Vec<Object> = Vec::with_capacity(page_order.len());

    for original_id in page_order {
        let mut page = page_objects
            .get(&original_id)
            .ok_or_else(|| {
                PdfError::Internal(format!(
                    "selected page {original_id:?} vanished during assembly"
                ))
            })?
            .clone();

        page.set("Parent", Object::Reference(pages_id));

        let id = if emitted.insert(original_id) {
            original_id
        } else {
            target.new_object_id()
        };

        target.objects.insert(id, Object::Dictionary(page));
        kids.push(Object::Reference(id));
    }

    let mut pages_dict = pages_node.map(|(_, dict)| dict).unwrap_or_default();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Count", Object::Integer(kids.len() as i64));
    pages_dict.set("Kids", Object::Array(kids));
    // Every page now carries its own copy of these, so leaving the originals on
    // the shared parent would only invite them to be inherited by the wrong page.
    for key in INHERITABLE {
        pages_dict.remove(key);
    }
    pages_dict.remove(b"Parent");
    target
        .objects
        .insert(pages_id, Object::Dictionary(pages_dict));

    let (catalog_id, mut catalog_dict) = match catalog {
        Some((id, dict)) => (id, dict),
        None => (target.new_object_id(), Dictionary::new()),
    };
    catalog_dict.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog_dict.set("Pages", Object::Reference(pages_id));
    for key in POSITION_DEPENDENT_CATALOG_KEYS {
        catalog_dict.remove(key);
    }
    target
        .objects
        .insert(catalog_id, Object::Dictionary(catalog_dict));

    target.trailer.set("Root", Object::Reference(catalog_id));
    match info_id.filter(|id| target.objects.contains_key(id)) {
        Some(id) => target.trailer.set("Info", Object::Reference(id)),
        None => {
            target.trailer.remove(b"Info");
        }
    }

    // Discarded pages leave their content streams and resources behind.
    target.prune_objects();
    target.renumber_objects();

    Ok(target)
}

/// Copy inherited attributes from the page tree down onto each page.
///
/// After this, every page carries its own `/Resources`, `/MediaBox`,
/// `/CropBox`, and `/Rotate` where an ancestor supplied them, which makes it
/// safe to give the page a different parent.
pub(crate) fn materialize_inherited_attributes(doc: &mut lopdf::Document) -> Result<()> {
    /// Depth limit for walking up the page tree; a malformed document can
    /// contain a `/Parent` cycle.
    const MAX_DEPTH: usize = 64;

    let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
    let mut updates: Vec<(ObjectId, InheritedAttributes)> = Vec::new();

    for page_id in page_ids {
        let Ok(page) = doc.get_dictionary(page_id) else {
            continue;
        };

        let mut missing: Vec<&'static [u8]> = INHERITABLE
            .iter()
            .copied()
            .filter(|key| page.get(key).is_err())
            .collect();

        if missing.is_empty() {
            continue;
        }

        let mut inherited: InheritedAttributes = Vec::new();
        let mut parent = page
            .get(b"Parent")
            .ok()
            .and_then(|object| object.as_reference().ok());
        let mut visited: HashSet<ObjectId> = HashSet::new();

        for _ in 0..MAX_DEPTH {
            let Some(parent_id) = parent else { break };
            if !visited.insert(parent_id) {
                break;
            }
            let Ok(dict) = doc.get_dictionary(parent_id) else {
                break;
            };

            missing.retain(|key| match dict.get(key) {
                Ok(value) => {
                    inherited.push((key, value.clone()));
                    false
                }
                Err(_) => true,
            });

            if missing.is_empty() {
                break;
            }

            parent = dict
                .get(b"Parent")
                .ok()
                .and_then(|object| object.as_reference().ok());
        }

        if !inherited.is_empty() {
            updates.push((page_id, inherited));
        }
    }

    for (page_id, attributes) in updates {
        let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
        for (key, value) in attributes {
            page.set(key.to_vec(), value);
        }
    }

    Ok(())
}

/// Compare two PDF version strings such as `"1.4"` and `"1.7"`.
///
/// Unparseable versions are treated as older, so a sane version always wins.
fn version_is_older(current: &str, candidate: &str) -> bool {
    fn parts(version: &str) -> Option<(u32, u32)> {
        let (major, minor) = version.trim().split_once('.')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    }

    match (parts(current), parts(candidate)) {
        (Some(current), Some(candidate)) => current < candidate,
        (None, Some(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_win() {
        assert!(version_is_older("1.4", "1.7"));
        assert!(version_is_older("1.7", "2.0"));
        assert!(!version_is_older("1.7", "1.4"));
        assert!(!version_is_older("1.7", "1.7"));
    }

    #[test]
    fn unparseable_versions_lose() {
        assert!(version_is_older("nonsense", "1.5"));
        assert!(!version_is_older("1.5", "nonsense"));
    }
}
