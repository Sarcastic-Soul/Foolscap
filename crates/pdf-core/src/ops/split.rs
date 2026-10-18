use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lopdf::Object;

use crate::assemble::{assemble, Source};
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

fn io_error(e: std::io::Error) -> PdfError {
    PdfError::Io {
        path: PathBuf::from("in-memory"),
        source: e,
    }
}

/// How to divide a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitSpec {
    /// Extract the selected pages into one output file, in the order given.
    Extract(PageRange),
    /// Burst into consecutive chunks of `n` pages each. The final chunk may be
    /// shorter.
    Every(usize),
    /// Split at each top-level bookmark (outline entry). Each output chunk
    /// contains the pages spanned by one bookmark and its descendants.
    ByBookmarks,
    /// Pack pages greedily into chunks not exceeding `max_mb` megabytes each.
    /// A single page larger than the limit becomes its own chunk.
    BySize(u64),
    /// Synonym for [`Every`] — burst every `n` pages. Kept for CLI clarity.
    ByPages(usize),
}

/// Split `input` according to `spec`, writing results into `out_dir`.
///
/// Returns the paths written, in order. Output files are named after the input
/// stem: `report-extract.pdf` for an extraction, `report-001.pdf` and onwards
/// for a burst.
pub fn split(input: &Path, spec: &SplitSpec, out_dir: &Path) -> Result<Vec<PathBuf>> {
    split_with_progress(input, spec, out_dir, None, None)
}

/// The paths [`split`] would write, without writing anything.
///
/// Callers that want to ask before overwriting need the names in advance;
/// deriving them here rather than in the caller keeps one naming scheme.
/// Counting the pages means opening the document, hence the `password`.
pub fn plan(
    input: &Path,
    spec: &SplitSpec,
    out_dir: &Path,
    password: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let source_doc = Document::open_with(input, password)?;
    let chunks = chunks(spec, &source_doc)?;
    Ok(output_paths(input, spec, out_dir, chunks.len()))
}

/// [`split`], reporting one progress tick per output file written.
///
/// `password` is used only if `input` needs one. The pieces are written
/// unencrypted.
pub fn split_with_progress(
    input: &Path,
    spec: &SplitSpec,
    out_dir: &Path,
    password: Option<&str>,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<Vec<PathBuf>> {
    let source_doc = Document::open_with(input, password)?;
    let chunks = chunks(spec, &source_doc)?;

    let total = chunks.len();
    let paths = output_paths(input, spec, out_dir, total);
    let mut written = Vec::with_capacity(total);

    for (index, (pages, path)) in chunks.into_iter().zip(paths).enumerate() {
        if let Some(report) = progress.as_mut() {
            report(Progress::new(
                index,
                Some(total),
                format!("writing {}", path.display()),
            ));
        }

        // Each output needs its own copy of the source: assembly consumes the
        // document it is given. Cloning is cheaper than parsing, and
        // decrypting, the file again.
        let assembled = assemble(vec![Source {
            doc: source_doc.inner.clone(),
            pages,
        }])?;
        Document::from_lopdf(assembled, None).save(&path)?;
        written.push(path);
    }

    if let Some(report) = progress.as_mut() {
        report(Progress::new(total, Some(total), "done"));
    }

    Ok(written)
}

/// Group the document's pages into one selection per output file.
fn chunks(spec: &SplitSpec, doc: &Document) -> Result<Vec<Vec<usize>>> {
    let page_count = doc.page_count();
    let chunks: Vec<Vec<usize>> = match spec {
        SplitSpec::Extract(range) => vec![range.resolve(page_count)?],
        SplitSpec::Every(size) => {
            if *size == 0 {
                return Err(PdfError::InvalidPageRange {
                    spec: "--every 0".into(),
                    reason: "chunk size must be at least 1".into(),
                });
            }
            (0..page_count)
                .collect::<Vec<_>>()
                .chunks(*size)
                .map(<[usize]>::to_vec)
                .collect()
        }
        SplitSpec::ByPages(size) => {
            if *size == 0 {
                return Err(PdfError::InvalidPageRange {
                    spec: "--by-pages 0".into(),
                    reason: "chunk size must be at least 1".into(),
                });
            }
            (0..page_count)
                .collect::<Vec<_>>()
                .chunks(*size)
                .map(<[usize]>::to_vec)
                .collect()
        }
        SplitSpec::ByBookmarks => bookmark_chunks(doc)?,
        SplitSpec::BySize(max_mb) => size_chunks(doc, *max_mb)?,
    };

    if chunks.iter().all(Vec::is_empty) {
        return Err(PdfError::EmptySelection);
    }

    Ok(chunks)
}

/// Split by top-level bookmarks (outline entries).
///
/// Each top-level bookmark starts a chunk that runs up to the page of the next
/// one. Pages before the first bookmark form a leading chunk of their own. A
/// document with no usable bookmarks comes back as a single chunk.
fn bookmark_chunks(doc: &Document) -> Result<Vec<Vec<usize>>> {
    let page_count = doc.page_count();
    let inner = &doc.inner;

    // Map page ObjectId -> zero-indexed page number.
    let page_index: HashMap<lopdf::ObjectId, usize> = inner
        .get_pages()
        .into_values()
        .enumerate()
        .map(|(index, id)| (id, index))
        .collect();

    let whole = || Ok(vec![(0..page_count).collect()]);

    // The outline root hangs off the catalog.
    let Some(catalog) = inner
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| inner.dereference(root).ok())
        .and_then(|(_, root)| root.as_dict().ok())
    else {
        return whole();
    };
    let Some(root) = catalog
        .get(b"Outlines")
        .ok()
        .and_then(|outlines| inner.dereference(outlines).ok())
        .and_then(|(_, outlines)| outlines.as_dict().ok())
    else {
        return whole();
    };

    // Walk the top-level siblings. The visited set stops a damaged file whose
    // Next links form a loop.
    let mut starts: Vec<usize> = Vec::new();
    let mut visited = HashSet::new();
    let mut current = root.get(b"First").ok().and_then(|o| o.as_reference().ok());
    while let Some(id) = current {
        if !visited.insert(id) {
            break;
        }
        let Ok(entry) = inner.get_dictionary(id) else {
            break;
        };
        if let Some(page) = bookmark_page(inner, catalog, entry, &page_index) {
            starts.push(page);
        }
        current = entry.get(b"Next").ok().and_then(|o| o.as_reference().ok());
    }

    starts.sort_unstable();
    starts.dedup();
    if starts.is_empty() {
        return whole();
    }

    let mut chunks: Vec<Vec<usize>> = Vec::new();
    if starts[0] > 0 {
        chunks.push((0..starts[0]).collect());
    }
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(page_count);
        chunks.push((*start..end).collect());
    }

    Ok(chunks)
}

/// The page a bookmark points at, from its `/Dest` or its `/A` action.
fn bookmark_page(
    doc: &lopdf::Document,
    catalog: &lopdf::Dictionary,
    entry: &lopdf::Dictionary,
    page_index: &HashMap<lopdf::ObjectId, usize>,
) -> Option<usize> {
    let dest = match entry.get(b"Dest") {
        Ok(dest) => dest,
        Err(_) => {
            let action = doc
                .dereference(entry.get(b"A").ok()?)
                .ok()?
                .1
                .as_dict()
                .ok()?;
            action.get(b"D").ok()?
        }
    };
    destination_page(doc, catalog, dest, page_index, 0)
}

/// Resolve a destination: an explicit array, a dictionary holding one under
/// `/D`, or a name looked up in the document's named destinations.
fn destination_page(
    doc: &lopdf::Document,
    catalog: &lopdf::Dictionary,
    dest: &Object,
    page_index: &HashMap<lopdf::ObjectId, usize>,
    depth: usize,
) -> Option<usize> {
    // Named destinations can point at each other in a damaged file.
    if depth > 4 {
        return None;
    }
    let (_, dest) = doc.dereference(dest).ok()?;
    match dest {
        Object::Array(array) => {
            let page = array.first()?.as_reference().ok()?;
            page_index.get(&page).copied()
        }
        Object::Dictionary(dict) => {
            destination_page(doc, catalog, dict.get(b"D").ok()?, page_index, depth + 1)
        }
        Object::Name(name) => {
            let target = named_destination(doc, catalog, name)?;
            destination_page(doc, catalog, &target, page_index, depth + 1)
        }
        Object::String(name, _) => {
            let target = named_destination(doc, catalog, name)?;
            destination_page(doc, catalog, &target, page_index, depth + 1)
        }
        _ => None,
    }
}

/// Look a destination name up in the catalog's `/Dests` dictionary, then in
/// the `/Names` `/Dests` name tree.
fn named_destination(
    doc: &lopdf::Document,
    catalog: &lopdf::Dictionary,
    name: &[u8],
) -> Option<Object> {
    if let Some(dests) = catalog
        .get(b"Dests")
        .ok()
        .and_then(|dests| doc.dereference(dests).ok())
        .and_then(|(_, dests)| dests.as_dict().ok())
    {
        if let Ok(found) = dests.get(name) {
            return Some(found.clone());
        }
    }

    let names = catalog.get(b"Names").ok()?;
    let names = doc.dereference(names).ok()?.1.as_dict().ok()?;
    let tree = doc
        .dereference(names.get(b"Dests").ok()?)
        .ok()?
        .1
        .as_dict()
        .ok()?;
    search_name_tree(doc, tree, name, 0)
}

fn search_name_tree(
    doc: &lopdf::Document,
    node: &lopdf::Dictionary,
    name: &[u8],
    depth: usize,
) -> Option<Object> {
    if depth > 16 {
        return None;
    }
    if let Some(pairs) = node
        .get(b"Names")
        .ok()
        .and_then(|names| doc.dereference(names).ok())
        .and_then(|(_, names)| names.as_array().ok())
    {
        for pair in pairs.chunks_exact(2) {
            if let Object::String(key, _) = &pair[0] {
                if key == name {
                    return Some(pair[1].clone());
                }
            }
        }
    }
    let kids = node
        .get(b"Kids")
        .ok()
        .and_then(|kids| doc.dereference(kids).ok())
        .and_then(|(_, kids)| kids.as_array().ok())?;
    kids.iter().find_map(|kid| {
        let kid = doc.dereference(kid).ok()?.1.as_dict().ok()?;
        search_name_tree(doc, kid, name, depth + 1)
    })
}

/// Split by approximate size: greedily pack pages into chunks not exceeding max_mb.
fn size_chunks(doc: &Document, max_mb: u64) -> Result<Vec<Vec<usize>>> {
    if max_mb == 0 {
        return Err(PdfError::InvalidPageRange {
            spec: "--max-size 0".into(),
            reason: "maximum size must be at least 1 MB".into(),
        });
    }

    let max_bytes = max_mb.saturating_mul(1_000_000);
    let page_count = doc.page_count();

    // Estimate size of each page by serializing just that page.
    // This is approximate but good enough for packing.
    let mut page_sizes = Vec::with_capacity(page_count);
    for i in 0..page_count {
        let mut assembled = assemble(vec![Source {
            doc: doc.inner.clone(),
            pages: vec![i],
        }])?;
        let mut buf = Vec::new();
        assembled.save_to(&mut buf).map_err(io_error)?;
        let size = buf.len() as u64;
        page_sizes.push(size);
    }

    let mut chunks = Vec::new();
    let mut current_chunk = Vec::new();
    let mut current_size = 0u64;

    for (i, &size) in page_sizes.iter().enumerate() {
        // If a single page exceeds the limit, it gets its own chunk.
        if size > max_bytes {
            if !current_chunk.is_empty() {
                chunks.push(current_chunk);
                current_chunk = Vec::new();
                current_size = 0;
            }
            chunks.push(vec![i]);
            continue;
        }

        if current_size + size > max_bytes && !current_chunk.is_empty() {
            chunks.push(current_chunk);
            current_chunk = Vec::new();
            current_size = 0;
        }

        current_chunk.push(i);
        current_size += size;
    }

    if !current_chunk.is_empty() {
        chunks.push(current_chunk);
    }

    Ok(chunks)
}

/// Derive the output file names for a split of `count` pieces.
fn output_paths(input: &Path, spec: &SplitSpec, out_dir: &Path, count: usize) -> Vec<PathBuf> {
    let stem = file_stem(input);
    let width = digits(count);

    (0..count)
        .map(|index| {
            let name = match spec {
                SplitSpec::Extract(_) => format!("{stem}-extract.pdf"),
                SplitSpec::Every(_) | SplitSpec::ByPages(_) => {
                    format!("{stem}-{:0width$}.pdf", index + 1, width = width)
                }
                SplitSpec::ByBookmarks => {
                    format!("{stem}-bookmark-{:0width$}.pdf", index + 1, width = width)
                }
                SplitSpec::BySize(_) => {
                    format!("{stem}-part-{:0width$}.pdf", index + 1, width = width)
                }
            };
            out_dir.join(name)
        })
        .collect()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_string())
}

fn digits(value: usize) -> usize {
    value.max(1).to_string().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names_are_zero_padded_to_the_chunk_count() {
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(100), 3);
        assert_eq!(digits(0), 1);
    }

    #[test]
    fn stem_falls_back_when_the_path_has_none() {
        assert_eq!(file_stem(Path::new("report.pdf")), "report");
        assert_eq!(file_stem(Path::new("/tmp/a.b.pdf")), "a.b");
        assert_eq!(file_stem(Path::new("/")), "document");
    }
}
