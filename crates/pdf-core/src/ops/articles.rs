//! Article threads: the reading-order chains of boxes (beads) that some
//! layout tools write into `/Threads`. This module lists them. Nothing here
//! edits a thread.

use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Object, ObjectId};

use crate::document::Document;
use crate::error::Result;

/// Guards against a bead chain that never returns to its start.
const MAX_BEADS: usize = 100_000;

/// One article: its title, how many boxes it spans, and where it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Article {
    pub title: String,
    pub beads: usize,
    /// The page of the first bead, one-indexed, when the bead names a page.
    pub first_page: Option<usize>,
}

/// List the article threads. A document without any has an empty list.
///
/// Threads that cannot be read are skipped, as bookmarks are: the list is
/// informational, so one damaged thread does not hide the rest.
pub fn list_articles(doc: &Document) -> Result<Vec<Article>> {
    let inner = &doc.inner;
    let Some(threads) = inner
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"Threads").ok())
        .and_then(|object| inner.dereference(object).ok())
        .and_then(|(_, object)| object.as_array().ok())
    else {
        return Ok(Vec::new());
    };

    let pages: HashMap<ObjectId, usize> = inner
        .get_pages()
        .into_iter()
        .map(|(number, id)| (id, number as usize))
        .collect();

    let mut out = Vec::new();
    for thread in threads {
        let Some(dict) = dict_at(inner, thread) else {
            continue;
        };
        let title = dict
            .get(b"I")
            .ok()
            .and_then(|info| dict_at(inner, info))
            .and_then(|info| info.get(b"Title").ok())
            .and_then(|object| match object {
                Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
                _ => None,
            })
            .unwrap_or_else(|| "Untitled".to_string());

        let Some(first) = dict
            .get(b"F")
            .ok()
            .and_then(|object| object.as_reference().ok())
        else {
            continue;
        };
        let (beads, first_page) = walk_beads(inner, first, &pages);
        out.push(Article {
            title,
            beads,
            first_page,
        });
    }
    Ok(out)
}

/// Count the beads from `first` around the cycle, and find the first bead's page.
fn walk_beads(
    inner: &lopdf::Document,
    first: ObjectId,
    pages: &HashMap<ObjectId, usize>,
) -> (usize, Option<usize>) {
    let first_page = inner
        .get_dictionary(first)
        .ok()
        .and_then(|bead| bead.get(b"P").ok())
        .and_then(|object| object.as_reference().ok())
        .and_then(|page| pages.get(&page).copied());

    let mut seen = HashSet::new();
    let mut current = Some(first);
    while let Some(id) = current {
        if !seen.insert(id) || seen.len() > MAX_BEADS {
            break;
        }
        current = inner
            .get_dictionary(id)
            .ok()
            .and_then(|bead| bead.get(b"N").ok())
            .and_then(|object| object.as_reference().ok());
    }
    (seen.len(), first_page)
}

fn dict_at<'a>(inner: &'a lopdf::Document, object: &'a Object) -> Option<&'a Dictionary> {
    inner.dereference(object).ok()?.1.as_dict().ok()
}
