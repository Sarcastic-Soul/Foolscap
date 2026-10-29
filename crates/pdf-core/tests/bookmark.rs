//! Integration tests for the outline/bookmark operations.
//!
//! Bookmarks are written to real files and read back with a fresh parse, so
//! what is asserted is what a viewer opening the saved document would see.
//! Some fixtures carry outlines written the way other producers write them —
//! `/A` actions, named destinations — so reading is checked against shapes
//! Foolscap did not write itself.

mod common;

use std::path::{Path, PathBuf};

use common::{Attributes, Workspace, A4};
use lopdf::{dictionary, Object, ObjectId};
use pdf_core::{
    add_bookmark, export_bookmarks, import_bookmarks, list_bookmarks, Bookmark, Document, PdfError,
};

// ---------------------------------------------------------------- helpers

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

/// Whole-tree comparison; `Bookmark` has no `PartialEq`, and the debug string
/// shows exactly which field disagrees.
fn assert_same_tree(actual: &[Bookmark], expected: &[Bookmark]) {
    assert_eq!(
        format!("{actual:#?}"),
        format!("{expected:#?}"),
        "listed tree differs from the expected one"
    );
}

/// A saved `page_count`-page document with no outline.
fn plain_document(name: &str, page_count: usize) -> (Workspace, PathBuf) {
    let workspace = Workspace::new();
    let path = workspace.document(name, page_count, "Bm");
    (workspace, path)
}

fn open(path: &Path) -> Document {
    Document::open(path).expect("fixture should open")
}

/// The tree `build_sample` produces: two levels of nesting, non-ASCII text,
/// and a parent whose child was appended after a deeper subtree existed.
fn sample_tree() -> Vec<Bookmark> {
    vec![
        node("Cover", 1, vec![bookmark("Inside cover", 6)]),
        node(
            "Chapter 1",
            2,
            vec![
                node("Section 1.1", 3, vec![bookmark("Subsection 1.1.1", 4)]),
                bookmark("Résumé — 第2節", 5),
            ],
        ),
        bookmark("Chapter 2", 6),
    ]
}

/// Build `sample_tree` in `doc` one `add` at a time, numbering the parents
/// the way `foolscap bookmark list` prints them.
fn build_sample(doc: &mut Document) {
    let steps: &[(&str, usize, usize)] = &[
        ("Cover", 1, 0),
        ("Chapter 1", 2, 0),
        ("Section 1.1", 3, 2),      // under [1] Chapter 1
        ("Subsection 1.1.1", 4, 3), // under [2] Section 1.1
        ("Résumé — 第2節", 5, 2),   // appended to Chapter 1's children
        ("Chapter 2", 6, 0),
        ("Inside cover", 6, 1), // under [0] Cover
    ];
    for (title, page, parent) in steps {
        add_bookmark(doc, bookmark(title, *page), *parent)
            .unwrap_or_else(|error| panic!("adding {title:?} under {parent} failed: {error}"));
    }
}

// --------------------------------------------- crafted producer outlines

/// How a crafted outline item names its destination.
enum Dest {
    /// An explicit `[page /XYZ …]` array on the item.
    Page(usize),
    /// A `/GoTo` action carrying the destination — the form most producers
    /// write.
    Action(usize),
    /// A name resolved through the catalog's `/Dests` dictionary, pointing at
    /// this page.
    Named(&'static str, usize),
    /// A name resolved through a `/Names /Dests` name tree whose values are
    /// the old-form `<< /D [page …] >>` dictionaries — the shape groff and
    /// other producers still write.
    NamedTree(&'static str, usize),
    /// A destination that resolves to no page of the document.
    Broken,
}

/// One crafted outline item: a title, how it points at a page, and children.
struct Item {
    title: &'static str,
    dest: Dest,
    children: Vec<Item>,
    /// Store the title and the action as separate objects, referenced
    /// indirectly — the shape some producers write.
    indirect: bool,
}

fn item(title: &'static str, dest: Dest) -> Item {
    Item {
        title,
        dest,
        children: Vec::new(),
        indirect: false,
    }
}

/// An item whose `/Title` and `/A` are indirect references.
fn indirect_item(title: &'static str, dest: Dest) -> Item {
    Item {
        indirect: true,
        ..item(title, dest)
    }
}

fn child(mut parent: Item, children: Vec<Item>) -> Item {
    parent.children = children;
    parent
}

/// A saved multi-page document carrying an outline written by hand, in the
/// shapes other producers emit.
fn outlined_document(name: &str, page_count: usize, items: &[Item]) -> (Workspace, PathBuf) {
    let mut doc = common::build(page_count, "Bm", A4, Attributes::OnPage);
    attach_outline(&mut doc, items);

    let workspace = Workspace::new();
    let path = workspace.write(name, doc);
    (workspace, path)
}

/// Build the `/Outlines` tree — and the `/Dests` table for named items —
/// directly on a generated document.
fn attach_outline(doc: &mut lopdf::Document, items: &[Item]) {
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();

    let mut named = Vec::new();
    collect_named(items, &mut named);
    if !named.is_empty() {
        let mut dests = dictionary! {};
        for (name, page) in &named {
            dests.set(
                name.as_bytes(),
                Object::Array(vec![
                    Object::Reference(pages[page - 1]),
                    Object::Name(b"Fit".to_vec()),
                ]),
            );
        }
        let dests_id = doc.add_object(dests);
        catalog_mut(doc).set("Dests", dests_id);
    }

    let mut tree_named = Vec::new();
    collect_tree_named(items, &mut tree_named);
    if !tree_named.is_empty() {
        let mut names_array = Vec::new();
        for (name, page) in &tree_named {
            let dest = doc.add_object(dictionary! {
                "D" => Object::Array(vec![
                    Object::Reference(pages[page - 1]),
                    Object::Name(b"Fit".to_vec()),
                ]),
            });
            names_array.push(Object::string_literal(*name));
            names_array.push(Object::Reference(dest));
        }
        let leaf = doc.add_object(dictionary! {
            "Names" => Object::Array(names_array),
        });
        let dests = doc.add_object(dictionary! {
            "Kids" => vec![Object::Reference(leaf)],
        });
        catalog_mut(doc).set("Names", dictionary! { "Dests" => dests });
    }

    let root = doc.new_object_id();
    let ids: Vec<ObjectId> = items
        .iter()
        .map(|spec| add_item(doc, &pages, spec, root))
        .collect();
    link_siblings(doc, &ids);

    let mut outline = dictionary! {
        "Type" => "Outlines",
        "Count" => count_items(items) as i64,
    };
    if let Some(first) = ids.first() {
        outline.set("First", *first);
        outline.set("Last", *ids.last().expect("first implies last"));
    }
    doc.objects.insert(root, Object::Dictionary(outline));
    catalog_mut(doc).set("Outlines", root);
}

/// Create one outline item object and its subtree; returns the item's id.
fn add_item(
    doc: &mut lopdf::Document,
    pages: &[ObjectId],
    spec: &Item,
    parent: ObjectId,
) -> ObjectId {
    let title = if spec.indirect {
        let id = doc.add_object(Object::string_literal(spec.title));
        Object::Reference(id)
    } else {
        Object::string_literal(spec.title)
    };

    let mut dict = dictionary! {
        "Title" => title,
        "Parent" => parent,
    };

    match &spec.dest {
        Dest::Page(page) => {
            dict.set(
                "Dest",
                Object::Array(vec![
                    Object::Reference(pages[page - 1]),
                    Object::Name(b"XYZ".to_vec()),
                    Object::Null,
                    Object::Null,
                    Object::Null,
                ]),
            );
        }
        Dest::Action(page) => {
            let action = dictionary! {
                "S" => "GoTo",
                "D" => Object::Array(vec![
                    Object::Reference(pages[page - 1]),
                    Object::Name(b"Fit".to_vec()),
                ]),
            };
            let action = if spec.indirect {
                Object::Reference(doc.add_object(action))
            } else {
                Object::Dictionary(action)
            };
            dict.set("A", action);
        }
        Dest::Named(name, _) | Dest::NamedTree(name, _) => {
            dict.set("Dest", Object::string_literal(*name));
        }
        Dest::Broken => {
            let bogus = doc.add_object(dictionary! { "Type" => "NotAPage" });
            dict.set(
                "Dest",
                Object::Array(vec![
                    Object::Reference(bogus),
                    Object::Name(b"XYZ".to_vec()),
                    Object::Null,
                    Object::Null,
                    Object::Null,
                ]),
            );
        }
    }

    let id = doc.add_object(dict);
    if !spec.children.is_empty() {
        let children: Vec<ObjectId> = spec
            .children
            .iter()
            .map(|child| add_item(doc, pages, child, id))
            .collect();
        link_siblings(doc, &children);

        let dict = doc.get_dictionary_mut(id).expect("item just added");
        dict.set("First", children[0]);
        dict.set("Last", *children.last().expect("children is not empty"));
        dict.set("Count", children.len() as i64);
    }

    id
}

/// Chain siblings with `/Prev` and `/Next`.
fn link_siblings(doc: &mut lopdf::Document, ids: &[ObjectId]) {
    for pair in ids.windows(2) {
        doc.get_dictionary_mut(pair[0])
            .expect("item just added")
            .set("Next", pair[1]);
        doc.get_dictionary_mut(pair[1])
            .expect("item just added")
            .set("Prev", pair[0]);
    }
}

fn collect_named(items: &[Item], out: &mut Vec<(&'static str, usize)>) {
    for spec in items {
        if let Dest::Named(name, page) = spec.dest {
            out.push((name, page));
        }
        collect_named(&spec.children, out);
    }
}

fn collect_tree_named(items: &[Item], out: &mut Vec<(&'static str, usize)>) {
    for spec in items {
        if let Dest::NamedTree(name, page) = spec.dest {
            out.push((name, page));
        }
        collect_tree_named(&spec.children, out);
    }
}

fn count_items(items: &[Item]) -> usize {
    items
        .iter()
        .map(|spec| 1 + count_items(&spec.children))
        .sum()
}

/// The catalog dictionary of a generated document, mutably.
fn catalog_mut(doc: &mut lopdf::Document) -> &mut lopdf::Dictionary {
    let catalog_id = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("generated documents have a catalog");
    doc.get_dictionary_mut(catalog_id)
        .expect("the catalog exists")
}

// ---------------------------------------------------------------- tests

#[test]
fn a_document_without_an_outline_lists_no_bookmarks() {
    let (_workspace, path) = plain_document("plain.pdf", 3);
    let doc = open(&path);

    assert!(list_bookmarks(&doc).unwrap().is_empty());
}

#[test]
fn added_bookmarks_survive_a_save_and_reload() {
    let (workspace, path) = plain_document("added.pdf", 6);
    let mut doc = open(&path);

    build_sample(&mut doc);

    // The tree reads back identically before saving and after a save/reload
    // round trip; saving is where links are normalised.
    assert_same_tree(&list_bookmarks(&doc).unwrap(), &sample_tree());

    let out = workspace.join("reloaded.pdf");
    doc.save(&out).unwrap();
    assert_same_tree(&list_bookmarks(&open(&out)).unwrap(), &sample_tree());
}

#[test]
fn export_then_import_into_a_fresh_copy_reproduces_the_tree() {
    let (workspace, path) = plain_document("source.pdf", 6);
    let mut doc = open(&path);
    build_sample(&mut doc);
    let out = workspace.join("bookmarked.pdf");
    doc.save(&out).unwrap();

    let exported = export_bookmarks(&open(&out)).unwrap();
    let json = serde_json::to_string(&exported).unwrap();
    assert!(json.contains("Chapter 1"), "the JSON is {json}");

    let parsed: Vec<Bookmark> = serde_json::from_str(&json).unwrap();
    let mut fresh = open(&path);
    import_bookmarks(&mut fresh, parsed).unwrap();
    let imported = workspace.join("imported.pdf");
    fresh.save(&imported).unwrap();

    assert_same_tree(&list_bookmarks(&open(&imported)).unwrap(), &exported);
}

#[test]
fn import_replaces_the_existing_outline() {
    let (workspace, path) = plain_document("replace.pdf", 4);
    let mut doc = open(&path);

    add_bookmark(&mut doc, bookmark("Old", 1), 0).unwrap();
    add_bookmark(&mut doc, bookmark("Also old", 2), 1).unwrap();

    let replacement = vec![node("New", 3, vec![bookmark("New child", 4)])];
    import_bookmarks(&mut doc, replacement.clone()).unwrap();

    let out = workspace.join("replaced.pdf");
    doc.save(&out).unwrap();
    assert_same_tree(&list_bookmarks(&open(&out)).unwrap(), &replacement);
}

#[test]
fn importing_an_empty_list_clears_the_outline() {
    let (workspace, path) = plain_document("cleared.pdf", 3);
    let mut doc = open(&path);

    add_bookmark(&mut doc, bookmark("Doomed", 1), 0).unwrap();
    import_bookmarks(&mut doc, Vec::new()).unwrap();

    let out = workspace.join("cleared-out.pdf");
    doc.save(&out).unwrap();

    assert!(list_bookmarks(&open(&out)).unwrap().is_empty());
    let raw = lopdf::Document::load(&out).unwrap();
    assert!(
        raw.catalog().unwrap().get(b"Outlines").is_err(),
        "the saved catalog should carry no /Outlines"
    );
}

#[test]
fn indirect_titles_and_actions_resolve_to_pages() {
    // Some producers store /Title and /A as separate objects, referenced
    // indirectly; found in the wild, so the reader follows the references.
    let (_workspace, path) = outlined_document(
        "indirect.pdf",
        4,
        &[indirect_item("Elsewhere", Dest::Action(4))],
    );

    let listed = list_bookmarks(&open(&path)).unwrap();
    assert_same_tree(&listed, &[node("Elsewhere", 4, Vec::new())]);
}

#[test]
fn goto_actions_resolve_to_pages() {
    let (_workspace, path) = outlined_document(
        "actions.pdf",
        5,
        &[
            child(
                item("Opening", Dest::Action(3)),
                vec![item("Part", Dest::Action(5))],
            ),
            item("Closing", Dest::Action(1)),
        ],
    );

    let listed = list_bookmarks(&open(&path)).unwrap();
    let expected = vec![
        node("Opening", 3, vec![node("Part", 5, Vec::new())]),
        node("Closing", 1, Vec::new()),
    ];
    assert_same_tree(&listed, &expected);
}

#[test]
fn named_destinations_resolve_to_pages() {
    let (_workspace, path) = outlined_document(
        "named.pdf",
        4,
        &[item("Contents", Dest::Named("table-of-contents", 2))],
    );

    let listed = list_bookmarks(&open(&path)).unwrap();
    assert_same_tree(&listed, &[node("Contents", 2, Vec::new())]);
}

#[test]
fn name_tree_destinations_in_old_d_form_resolve_to_pages() {
    let (_workspace, path) = outlined_document(
        "named-tree.pdf",
        4,
        &[
            item("Tree entry", Dest::NamedTree("Link1", 4)),
            item("Plain sibling", Dest::Page(1)),
        ],
    );

    let listed = list_bookmarks(&open(&path)).unwrap();
    assert_same_tree(
        &listed,
        &[
            node("Tree entry", 4, Vec::new()),
            node("Plain sibling", 1, Vec::new()),
        ],
    );
}

#[test]
fn explicit_destination_arrays_resolve_to_pages() {
    let (_workspace, path) = outlined_document(
        "explicit.pdf",
        4,
        &[child(
            item("Middle", Dest::Page(3)),
            vec![item("Last", Dest::Page(4))],
        )],
    );

    let listed = list_bookmarks(&open(&path)).unwrap();
    assert_same_tree(
        &listed,
        &[node("Middle", 3, vec![node("Last", 4, Vec::new())])],
    );
}

#[test]
fn unresolvable_destinations_skip_the_item_and_its_children() {
    // The broken item's child has a perfectly good destination; it goes with
    // its parent, so the surviving numbering never mentions a dead branch.
    let (_workspace, path) = outlined_document(
        "broken.pdf",
        4,
        &[
            child(
                item("Lost", Dest::Broken),
                vec![item("Lost child", Dest::Page(1))],
            ),
            item("Kept", Dest::Page(2)),
        ],
    );

    let listed = list_bookmarks(&open(&path)).unwrap();
    assert_same_tree(&listed, &[node("Kept", 2, Vec::new())]);
}

#[test]
fn out_of_range_pages_are_rejected() {
    let (_workspace, path) = plain_document("pages.pdf", 3);
    let mut doc = open(&path);

    for bad_page in [0, 4] {
        let error = add_bookmark(&mut doc, bookmark("bad", bad_page), 0).unwrap_err();
        assert!(
            matches!(error, PdfError::PageOutOfRange { requested, total: 3 } if requested == bad_page),
            "page {bad_page} gave {error:?}"
        );
    }

    let error =
        import_bookmarks(&mut doc, vec![node("outer", 1, vec![bookmark("inner", 9)])]).unwrap_err();
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

    // Nothing was written along the way.
    assert!(list_bookmarks(&doc).unwrap().is_empty());
}

#[test]
fn an_out_of_range_parent_is_an_error() {
    let (_workspace, path) = plain_document("parent.pdf", 2);
    let mut doc = open(&path);

    let error = add_bookmark(&mut doc, bookmark("orphan", 1), 1).unwrap_err();
    assert!(matches!(error, PdfError::Internal(_)), "got {error:?}");
    assert!(list_bookmarks(&doc).unwrap().is_empty());
}

#[test]
fn bookmarks_can_be_added_to_an_existing_producer_outline() {
    let (workspace, path) = outlined_document(
        "producer.pdf",
        4,
        &[
            item("Old 1", Dest::Action(1)),
            item("Old 2", Dest::Action(2)),
        ],
    );

    let mut doc = open(&path);
    add_bookmark(&mut doc, bookmark("New", 4), 0).unwrap();
    add_bookmark(&mut doc, bookmark("Under old 1", 3), 1).unwrap();

    let out = workspace.join("extended.pdf");
    doc.save(&out).unwrap();

    let listed = list_bookmarks(&open(&out)).unwrap();
    let expected = vec![
        node("Old 1", 1, vec![node("Under old 1", 3, Vec::new())]),
        node("Old 2", 2, Vec::new()),
        node("New", 4, Vec::new()),
    ];
    assert_same_tree(&listed, &expected);
}

// ------------------------------------------------- remove and move (printed order)

fn titles(tree: &[Bookmark]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(tree: &[Bookmark], out: &mut Vec<String>) {
        for node in tree {
            out.push(node.title.clone());
            walk(&node.children, out);
        }
    }
    walk(tree, &mut out);
    out
}

#[test]
fn removing_a_bookmark_takes_its_children_with_it() {
    let (workspace, path) = plain_document("remove.pdf", 6);
    let mut doc = open(&path);
    for top in sample_tree() {
        add_bookmark(&mut doc, top, 0).unwrap();
    }
    // Printed order: Cover 0, Inside cover 1, Chapter 1 2 (with children below it).
    let removed = pdf_core::remove_bookmark(&mut doc, 2).unwrap();
    assert_eq!(removed.title, "Chapter 1");

    let saved = workspace.join("removed.pdf");
    doc.save(&saved).unwrap();
    let listed = list_bookmarks(&open(&saved)).unwrap();
    assert_eq!(titles(&listed), vec!["Cover", "Inside cover", "Chapter 2"]);
}

#[test]
fn moving_a_bookmark_swaps_it_with_its_sibling() {
    let (workspace, path) = plain_document("move.pdf", 6);
    let mut doc = open(&path);
    for top in sample_tree() {
        add_bookmark(&mut doc, top, 0).unwrap();
    }
    // Printed order puts "Chapter 2" last, at index 6. Moving it up swaps it with "Chapter 1".
    let chapter_two = 6;
    assert!(pdf_core::move_bookmark(&mut doc, chapter_two, true).unwrap());
    let saved = workspace.join("moved.pdf");
    doc.save(&saved).unwrap();

    let listed = list_bookmarks(&open(&saved)).unwrap();
    let top: Vec<&str> = listed.iter().map(|node| node.title.as_str()).collect();
    assert_eq!(top, vec!["Cover", "Chapter 2", "Chapter 1"]);
}

#[test]
fn moving_past_the_end_changes_nothing() {
    let (_workspace, path) = plain_document("edge.pdf", 6);
    let mut doc = open(&path);
    for top in sample_tree() {
        add_bookmark(&mut doc, top, 0).unwrap();
    }
    // "Cover" is first, so moving it up does nothing.
    assert!(!pdf_core::move_bookmark(&mut doc, 0, true).unwrap());
}

#[test]
fn an_unknown_index_is_an_error() {
    let (_workspace, path) = plain_document("missing.pdf", 6);
    let mut doc = open(&path);
    add_bookmark(&mut doc, bookmark("Only", 1), 0).unwrap();
    assert!(pdf_core::remove_bookmark(&mut doc, 9).is_err());
}
