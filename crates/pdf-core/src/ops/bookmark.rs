//! Document outlines (bookmarks).

use std::collections::{HashMap, HashSet};

use lopdf::{dictionary, Object, ObjectId, StringFormat};
use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::to_zero_indexed;
use crate::text;

/// One node of the outline tree. Pages are one-indexed, the way the user
/// writes them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub title: String,
    pub page: usize,
    #[serde(default)]
    pub children: Vec<Bookmark>,
}

impl Bookmark {
    pub fn new(title: String, page: usize) -> Self {
        Self {
            title,
            page,
            children: Vec::new(),
        }
    }
}

/// How many outline items to read before deciding the outline is circular.
const MAX_OUTLINE_ITEMS: usize = 10_000;
/// How deep to nest before deciding the same.
const MAX_OUTLINE_DEPTH: usize = 64;
/// How many hops a destination may take through named destinations and
/// indirect references before giving up on it.
const MAX_DEST_HOPS: usize = 16;

/// Read the document's outline tree, in reading order.
///
/// Items whose destination cannot be resolved are skipped, together with
/// everything under them, with a warning: a bookmark that points nowhere is
/// misleading, and half a table of contents beats one that refuses to open.
/// The outline is decoration; nothing here fails the whole listing.
pub fn list_bookmarks(doc: &Document) -> Result<Vec<Bookmark>> {
    let inner = &doc.inner;
    let catalog = inner.catalog()?;
    let Ok(outlines) = inner.get_dict_in_dict(catalog, b"Outlines") else {
        return Ok(Vec::new());
    };

    // Page object id -> one-indexed position, for resolving destinations.
    // `get_pages` walks the page tree in document order, which is what the
    // numbers must match.
    let pages: HashMap<ObjectId, usize> = inner
        .get_pages()
        .into_iter()
        .map(|(number, id)| (id, number as usize))
        .collect();
    let mut visited = HashSet::new();

    Ok(read_level(inner, &pages, &mut visited, outlines, 0))
}

/// Append `bookmark` under the top-level node `parent` (0 = top level).
///
/// `parent` numbers the outline the way `foolscap bookmark list` prints it:
/// pre-order, parents before their children, so `parent` N nests under the
/// bookmark printed as `N - 1`. The bookmark goes at the end of that node's
/// children.
pub fn add_bookmark(doc: &mut Document, bookmark: Bookmark, parent: usize) -> Result<()> {
    let mut tree = list_bookmarks(doc)?;
    insert_by_pre_order(&mut tree, bookmark, parent)?;
    write_outline(doc, &tree)
}

/// Read the outline tree, shaped for JSON export.
pub fn export_bookmarks(doc: &Document) -> Result<Vec<Bookmark>> {
    list_bookmarks(doc)
}

/// Replace the document's outline with `bookmarks`.
pub fn import_bookmarks(doc: &mut Document, bookmarks: Vec<Bookmark>) -> Result<()> {
    write_outline(doc, &bookmarks)
}

// ---------------------------------------------------------------- reading

/// Read one level of the outline: the `/First` child of `parent`, then the
/// `/Next` chain across its siblings.
fn read_level(
    inner: &lopdf::Document,
    pages: &HashMap<ObjectId, usize>,
    visited: &mut HashSet<ObjectId>,
    parent: &lopdf::Dictionary,
    depth: usize,
) -> Vec<Bookmark> {
    let mut out = Vec::new();

    let Some(mut current) = parent
        .get(b"First")
        .ok()
        .and_then(|object| object.as_reference().ok())
    else {
        return out;
    };

    loop {
        if !visited.insert(current) {
            tracing::warn!("outline items form a cycle; the rest is ignored");
            break;
        }
        if visited.len() > MAX_OUTLINE_ITEMS {
            tracing::warn!("outline has more than {MAX_OUTLINE_ITEMS} items; the rest is ignored");
            break;
        }

        let Ok(item) = inner.get_dictionary(current) else {
            tracing::warn!("outline item points at a missing object; the rest is ignored");
            break;
        };

        // Read the sibling link before recursing, while `item` is still
        // borrowed; an item whose destination does not resolve takes its
        // children with it.
        let next = item
            .get(b"Next")
            .ok()
            .and_then(|object| object.as_reference().ok());

        let title = item
            .get(b"Title")
            .ok()
            .and_then(|object| title_string(inner, object, MAX_DEST_HOPS));
        let page = destination_page(inner, pages, item);

        match (title, page) {
            (Some(title), Some(page)) => {
                let children = if depth < MAX_OUTLINE_DEPTH {
                    read_level(inner, pages, visited, item, depth + 1)
                } else {
                    tracing::warn!(
                        "outline nests deeper than {MAX_OUTLINE_DEPTH} levels; the rest is ignored"
                    );
                    Vec::new()
                };
                out.push(Bookmark {
                    title,
                    page,
                    children,
                });
            }
            _ => {
                tracing::warn!(
                    ?current,
                    "outline item has no usable title or destination; it and its children are skipped"
                );
            }
        }

        match next {
            Some(id) => current = id,
            None => break,
        }
    }

    out
}

/// Read an outline item's title, following indirect references — some
/// producers store `/Title` (and the whole action) as separate objects.
fn title_string(inner: &lopdf::Document, object: &Object, hops: usize) -> Option<String> {
    if hops == 0 {
        return None;
    }
    match object {
        Object::String(bytes, _) => Some(text::decode_pdf_string(bytes)),
        Object::Reference(id) => inner
            .get_object(*id)
            .ok()
            .and_then(|object| title_string(inner, object, hops - 1)),
        _ => None,
    }
}

/// The one-indexed page an outline item points at, if it can be resolved.
///
/// An explicit `/Dest` wins over an `/A` action when both are present; the
/// standard does not permit them together, and `/Dest` is the direct form.
/// Only `/GoTo` actions carry an in-document destination — anything else
/// points outside the file and is unresolvable here.
fn destination_page(
    inner: &lopdf::Document,
    pages: &HashMap<ObjectId, usize>,
    item: &lopdf::Dictionary,
) -> Option<usize> {
    let dest = match item.get(b"Dest") {
        Ok(dest) => dest,
        Err(_) => {
            let action = dict_entry(inner, item, b"A")?;
            let kind = action.get(b"S").ok()?.as_name().ok()?;
            if kind != b"GoTo" {
                return None;
            }
            action.get(b"D").ok()?
        }
    };

    resolve_destination(inner, pages, dest, MAX_DEST_HOPS)
}

/// Dereference `key` into a dictionary, following an indirect reference.
fn dict_entry<'a>(
    inner: &'a lopdf::Document,
    node: &'a lopdf::Dictionary,
    key: &[u8],
) -> Option<&'a lopdf::Dictionary> {
    match node.get(key).ok()? {
        Object::Dictionary(dict) => Some(dict),
        Object::Reference(id) => inner.get_dictionary(*id).ok(),
        _ => None,
    }
}

/// Resolve a destination object to a one-indexed page number.
///
/// Destinations arrive as explicit arrays (`[page /XYZ x y zoom]`), as names
/// to look up in the document's destination tables, or as indirect references
/// to either. `hops` bounds the indirect chains.
fn resolve_destination(
    inner: &lopdf::Document,
    pages: &HashMap<ObjectId, usize>,
    dest: &Object,
    hops: usize,
) -> Option<usize> {
    if hops == 0 {
        tracing::warn!("destination chain never reaches a page; the item is skipped");
        return None;
    }

    match dest {
        Object::Array(array) => match array.first()? {
            Object::Reference(id) => pages.get(id).copied(),
            // The standard says the first element is a page reference; a bare
            // integer is what naive writers emit when they mean page N.
            Object::Integer(page) => usize::try_from(*page)
                .ok()
                .filter(|page| (1..=pages.len()).contains(page)),
            _ => None,
        },
        Object::String(name, _) | Object::Name(name) => {
            let dest = named_destination(inner, name)?;
            resolve_destination(inner, pages, &dest, hops - 1)
        }
        Object::Reference(id) => {
            let dest = inner.get_object(*id).ok()?;
            resolve_destination(inner, pages, dest, hops - 1)
        }
        // The old named-destination form: a dictionary whose /D entry holds
        // the destination array. Current producers write the array directly,
        // but files in the wild (groff, among others) still carry this shape.
        Object::Dictionary(dict) => {
            let dest = dict.get(b"D").ok()?;
            resolve_destination(inner, pages, dest, hops - 1)
        }
        _ => None,
    }
}

/// Look `name` up in the document's named destinations: the catalog's `/Dests`
/// dictionary (the old form), then the `/Names /Dests` name tree.
fn named_destination(inner: &lopdf::Document, name: &[u8]) -> Option<Object> {
    let catalog = inner.catalog().ok()?;

    if let Some(dests) = dict_entry(inner, catalog, b"Dests") {
        if let Ok(dest) = dests.get(name) {
            return Some(dest.clone());
        }
    }

    let names = dict_entry(inner, catalog, b"Names")?;
    let tree = dict_entry(inner, names, b"Dests")?;
    find_in_name_tree(inner, tree, name, MAX_DEST_HOPS)
}

/// Search a name-tree node for `name`, descending through `/Kids`.
fn find_in_name_tree(
    inner: &lopdf::Document,
    node: &lopdf::Dictionary,
    name: &[u8],
    hops: usize,
) -> Option<Object> {
    if hops == 0 {
        return None;
    }

    // A leaf holds `/Names [key1 value1 key2 value2 …]`, keys as strings.
    if let Ok(Object::Array(entries)) = node.get(b"Names") {
        for pair in entries.chunks(2) {
            let key = match pair.first() {
                Some(Object::String(key, _)) => key,
                _ => continue,
            };
            if key.as_slice() == name {
                return pair.get(1).cloned();
            }
        }
    }

    if let Ok(Object::Array(kids)) = node.get(b"Kids") {
        for kid in kids {
            let Ok(id) = kid.as_reference() else {
                continue;
            };
            let Ok(tree) = inner.get_dictionary(id) else {
                continue;
            };
            if let Some(found) = find_in_name_tree(inner, tree, name, hops - 1) {
                return Some(found);
            }
        }
    }

    None
}

// ---------------------------------------------------------------- writing

/// Replace the document's outline with `tree`.
///
/// The outline is rebuilt whole rather than edited in place: [`add_bookmark`]
/// then cannot disagree with [`list_bookmarks`] about numbering, and
/// [`import_bookmarks`] needs the same construction anyway. The old items
/// become unreferenced; `optimize` collects them. Every destination is written
/// explicitly as `[page /XYZ null null null]`, and every item is open, so each
/// `/Count` is the number of descendants beneath it.
fn write_outline(doc: &mut Document, tree: &[Bookmark]) -> Result<()> {
    validate_pages(tree, doc.page_count())?;

    if tree.is_empty() {
        if let Ok(catalog) = doc.inner.catalog_mut() {
            catalog.remove(b"Outlines");
        }
        return Ok(());
    }

    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();
    let root = doc.inner.new_object_id();
    let (first, last, count) = write_level(&mut doc.inner, tree, root, &page_ids)?;

    let mut outlines = dictionary! {
        "Type" => "Outlines",
        "Count" => count,
    };
    if let Some(first) = first {
        outlines.set(b"First", Object::Reference(first));
    }
    if let Some(last) = last {
        outlines.set(b"Last", Object::Reference(last));
    }
    doc.inner.objects.insert(root, Object::Dictionary(outlines));
    doc.inner
        .catalog_mut()?
        .set(b"Outlines", Object::Reference(root));

    Ok(())
}

/// Write one level of siblings under `parent`, returning the first and last
/// item and the total number of items written, descendants included.
fn write_level(
    inner: &mut lopdf::Document,
    level: &[Bookmark],
    parent: ObjectId,
    page_ids: &[ObjectId],
) -> Result<(Option<ObjectId>, Option<ObjectId>, i64)> {
    let mut first = None;
    let mut last = None;
    let mut total: i64 = 0;

    for bookmark in level {
        let id = inner.new_object_id();
        let page = page_ids[to_zero_indexed(bookmark.page, page_ids.len())?];

        let mut item = dictionary! {
            "Title" => Object::String(
                text::encode_pdf_string(&bookmark.title),
                StringFormat::Literal,
            ),
            "Parent" => parent,
            "Dest" => Object::Array(vec![
                Object::Reference(page),
                Object::Name(b"XYZ".to_vec()),
                Object::Null,
                Object::Null,
                Object::Null,
            ]),
        };

        let mut descendants: i64 = 0;
        if !bookmark.children.is_empty() {
            let (child_first, child_last, child_total) =
                write_level(inner, &bookmark.children, id, page_ids)?;
            if let Some(child) = child_first {
                item.set(b"First", Object::Reference(child));
            }
            if let Some(child) = child_last {
                item.set(b"Last", Object::Reference(child));
            }
            item.set(b"Count", child_total);
            descendants = child_total;
        }

        if let Some(previous) = last {
            item.set(b"Prev", Object::Reference(previous));
        }

        inner.objects.insert(id, Object::Dictionary(item));

        if first.is_none() {
            first = Some(id);
        }
        if let Some(previous) = last {
            inner
                .get_dictionary_mut(previous)?
                .set(b"Next", Object::Reference(id));
        }
        last = Some(id);
        total += 1 + descendants;
    }

    Ok((first, last, total))
}

/// Bounds-check every page number before anything is written.
fn validate_pages(tree: &[Bookmark], page_count: usize) -> Result<()> {
    for bookmark in tree {
        to_zero_indexed(bookmark.page, page_count)?;
        validate_pages(&bookmark.children, page_count)?;
    }
    Ok(())
}

/// Append `bookmark` at the position named by the CLI's `--parent` numbering.
///
/// `parent` 0 appends at the top level; `parent` N appends to the children of
/// the bookmark printed as `N - 1` by `foolscap bookmark list` — pre-order
/// numbering, each parent counted before its children.
fn insert_by_pre_order(tree: &mut Vec<Bookmark>, bookmark: Bookmark, parent: usize) -> Result<()> {
    if parent == 0 {
        tree.push(bookmark);
        return Ok(());
    }

    let mut seen = 0;
    let Some(mut path) = path_to_index(tree, parent - 1, &mut seen) else {
        return Err(PdfError::Internal(format!(
            "no bookmark numbered {parent}; the outline has {seen} item(s)"
        )));
    };
    // The recursion returns the path leaf-first.
    path.reverse();

    let mut node = &mut tree[path[0]];
    for step in &path[1..] {
        node = &mut node.children[*step];
    }
    node.children.push(bookmark);

    Ok(())
}

/// The path of child indices from the top level to the bookmark at pre-order
/// `index`, if there is one. Each node is counted before its children, the
/// same order the CLI numbers bookmarks in when listing them.
fn path_to_index(level: &[Bookmark], index: usize, counter: &mut usize) -> Option<Vec<usize>> {
    for (position, node) in level.iter().enumerate() {
        let current = *counter;
        *counter += 1;

        if current == index {
            return Some(vec![position]);
        }
        if let Some(mut path) = path_to_index(&node.children, index, counter) {
            path.push(position);
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(title: &str, page: usize) -> Bookmark {
        Bookmark::new(title.to_string(), page)
    }

    fn node(title: &str, page: usize, children: Vec<Bookmark>) -> Bookmark {
        Bookmark {
            title: title.to_string(),
            page,
            children,
        }
    }

    /// The titles in pre-order, the order `foolscap bookmark list` numbers
    /// them in.
    fn pre_order(tree: &[Bookmark]) -> Vec<&str> {
        let mut titles = Vec::new();
        fn walk<'a>(level: &'a [Bookmark], titles: &mut Vec<&'a str>) {
            for bookmark in level {
                titles.push(&bookmark.title);
                walk(&bookmark.children, titles);
            }
        }
        walk(tree, &mut titles);
        titles
    }

    #[test]
    fn parent_zero_appends_at_the_top_level() {
        let mut tree = vec![bookmark("A", 1), bookmark("B", 2)];
        insert_by_pre_order(&mut tree, bookmark("C", 3), 0).unwrap();

        assert_eq!(pre_order(&tree), ["A", "B", "C"]);
        assert!(tree.iter().all(|node| node.children.is_empty()));
    }

    #[test]
    fn parent_one_nests_under_the_first_bookmark() {
        let mut tree = vec![bookmark("A", 1), bookmark("B", 2)];
        insert_by_pre_order(&mut tree, bookmark("C", 3), 1).unwrap();

        assert_eq!(pre_order(&tree), ["A", "C", "B"]);
        assert_eq!(tree[0].children.len(), 1);
    }

    #[test]
    fn numbering_counts_each_parent_before_its_children() {
        // Pre-order is A(0) A1(1) A2(2) B(3) — so parent 2 is A1 and parent 4
        // is B.
        let mut tree = vec![node("A", 1, vec![bookmark("A1", 1), bookmark("A2", 1)])];
        tree.push(bookmark("B", 2));

        insert_by_pre_order(&mut tree, bookmark("under A1", 1), 2).unwrap();
        assert_eq!(tree[0].children[0].children.len(), 1);
        assert_eq!(pre_order(&tree), ["A", "A1", "under A1", "A2", "B"]);

        // B is now printed as [4], so nesting under it takes parent 5.
        insert_by_pre_order(&mut tree, bookmark("under B", 2), 5).unwrap();
        assert_eq!(tree[1].children.len(), 1);
        assert_eq!(
            pre_order(&tree),
            ["A", "A1", "under A1", "A2", "B", "under B"]
        );
    }

    #[test]
    fn appends_go_to_the_end_of_the_parents_children() {
        let mut tree = vec![node("A", 1, vec![bookmark("first", 1)])];

        insert_by_pre_order(&mut tree, bookmark("second", 2), 1).unwrap();

        let children: Vec<&str> = tree[0].children.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(children, ["first", "second"]);
    }

    #[test]
    fn a_chain_can_be_built_from_parent_indices_alone() {
        // The first bookmark must land at the top level; after that, each new
        // bookmark nests under the one just added, which holds the previous
        // highest pre-order number.
        let mut tree = Vec::new();
        for (step, title) in ["one", "two", "three"].iter().enumerate() {
            let parent = if step == 0 { 0 } else { step };
            insert_by_pre_order(&mut tree, bookmark(title, 1), parent).unwrap();
        }

        assert_eq!(pre_order(&tree), ["one", "two", "three"]);
        assert_eq!(tree[0].children[0].children.len(), 1);
    }

    #[test]
    fn an_out_of_range_parent_is_an_error() {
        let mut tree = vec![bookmark("A", 1)];

        let error = insert_by_pre_order(&mut tree, bookmark("B", 1), 2).unwrap_err();
        assert!(matches!(error, PdfError::Internal(_)), "got {error:?}");
        assert_eq!(pre_order(&tree), ["A"]);

        // An empty outline has no bookmark 1 either.
        let mut empty = Vec::new();
        assert!(insert_by_pre_order(&mut empty, bookmark("B", 1), 1).is_err());
    }

    #[test]
    fn page_numbers_are_validated_before_anything_is_written() {
        let tree = vec![node("A", 0, vec![bookmark("A1", 2)])];
        assert!(matches!(
            validate_pages(&tree, 3),
            Err(PdfError::PageOutOfRange { requested: 0, .. })
        ));

        let tree = vec![node("A", 4, vec![bookmark("A1", 2)])];
        assert!(matches!(
            validate_pages(&tree, 3),
            Err(PdfError::PageOutOfRange { requested: 4, .. })
        ));

        let tree = vec![node("A", 3, vec![bookmark("A1", 1)])];
        assert!(validate_pages(&tree, 3).is_ok());
    }
}
