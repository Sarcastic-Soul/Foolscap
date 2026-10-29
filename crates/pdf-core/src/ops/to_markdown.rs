//! Extract structured text as Markdown.
//!
//! The text comes out of MuPDF's structured-text extractor, the same source
//! `pdf_to_html` reads: blocks of lines, each character carrying the font
//! size it was drawn at. Two heuristics turn that geometry into structure:
//!
//! - *Headings.* Font sizes are bucketed to half points and weighted by how
//!   much text is set in them; the heaviest bucket is the body size, and every
//!   bucket at least ten per cent larger is a heading level, largest first
//!   (`#`, then `##`, and so on, capped at six).
//! - *Lists.* A line starting with a bullet glyph, or indented a full em from
//!   the left edge of its block, becomes a `- ` item; a line hanging out under
//!   an item is treated as that item's wrapped tail.
//!
//! Everything else is a paragraph. Blocks are separated by one blank line,
//! and pages contribute nothing but their content: no page markers.

use std::path::{Path, PathBuf};

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;

/// Options for [`to_markdown`].
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownOptions {
    /// Pages to convert, one-indexed as the user writes them; `None` converts
    /// every page.
    pub pages: Option<PageRange>,
    /// Infer heading levels from font-size clusters. When off, every line is
    /// body text.
    pub headings: bool,
    /// Infer list items from bullet glyphs and hanging indents. When off,
    /// bulleted text becomes ordinary paragraphs.
    pub lists: bool,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            pages: None,
            headings: true,
            lists: true,
        }
    }
}

/// Markdown has six heading levels.
const MAX_HEADING_LEVEL: usize = 6;
/// A font size must exceed the body size by this factor to count as a
/// heading, so rounding noise on body text does not manufacture levels.
const HEADING_MARGIN: f32 = 1.1;
/// Glyphs a line may start with that make it a list item. The ASCII dash and
/// en dash are included for documents whose bullets were typed by hand; every
/// one of them must be followed by a space.
const BULLETS: [char; 12] = [
    '\u{2022}', '\u{25CF}', '\u{25E6}', '\u{25AA}', '\u{25AB}', '\u{25A0}', '\u{25A1}', '\u{2023}',
    '\u{2043}', '\u{00B7}', '-', '\u{2013}',
];

/// One extracted line of text with the geometry the heuristics need.
#[derive(Debug, Clone)]
struct Line {
    text: String,
    /// Dominant font size in points.
    size: f32,
    /// Left edge of the line in page coordinates.
    x0: f32,
}

/// Render the document's text as Markdown, with headings and lists inferred
/// from font size and indentation.
///
/// Needs the `render` feature: the text is read through MuPDF, which opens
/// files rather than in-memory documents. A decrypted copy of `doc` is
/// therefore written to a scratch file first, so the conversion sees exactly
/// the document the caller holds — including any modifications made since it
/// was opened — without ever needing its password.
pub fn to_markdown(doc: &Document, options: &MarkdownOptions) -> Result<String> {
    let scratch = scratch_copy(doc)?;
    let rendered = render(&scratch, options);
    // Best effort: a leftover scratch file in the temp directory is untidy,
    // not a failure of the extraction.
    let _ = std::fs::remove_file(&scratch);
    rendered
}

/// Write a copy of the document for MuPDF to open.
fn scratch_copy(doc: &Document) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let path = std::env::temp_dir().join(format!(
        "foolscap-to-markdown-{}-{}.pdf",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));

    // A clone, because saving appends objects to the document it is given,
    // and a read-only extraction must not modify the caller's document.
    let mut copy = doc.inner.clone();
    copy.save(&path).map_err(|source| PdfError::Io {
        path: path.clone(),
        source: std::io::Error::other(source.to_string()),
    })?;

    Ok(path)
}

/// Read every selected page of the file at `path` and lay out its Markdown.
fn render(path: &Path, options: &MarkdownOptions) -> Result<String> {
    let source = mupdf::Document::open(path).map_err(|source| mupdf_error(path, source))?;
    let page_count = source
        .page_count()
        .map_err(|error| mupdf_error(path, error))?
        .max(0) as usize;

    let selected = match &options.pages {
        Some(range) => range.resolve(page_count)?,
        None => (0..page_count).collect::<Vec<_>>(),
    };

    // Pass one gathers lines and geometry; the size histogram is only known
    // once every page has been read, so nothing is emitted yet.
    let mut pages: Vec<Vec<Vec<Line>>> = Vec::with_capacity(selected.len());
    for page in &selected {
        let loaded = source
            .load_page(*page as i32)
            .map_err(|error| mupdf_error(path, error))?;
        let text_page = loaded
            .to_text_page(mupdf::TextPageFlags::empty())
            .map_err(|error| mupdf_error(path, error))?;
        let structured = text_page.structured();

        let mut blocks = Vec::new();
        for block in &structured.blocks {
            if let mupdf::TextBlockContent::Text { lines } = &block.content {
                let lines: Vec<Line> = lines.iter().filter_map(extracted_line).collect();
                if !lines.is_empty() {
                    blocks.push(lines);
                }
            }
        }
        pages.push(blocks);
    }

    // Pass two classifies and emits. Blocks already carry a blank line
    // between them, which is also what separates one page's content from the
    // next: pages leave no marker of their own.
    let sizes = SizeMap::build(pages.iter().flatten().flatten());
    tracing::debug!(
        body_size = sizes.body,
        heading_sizes = ?sizes.headings,
        "font-size clusters behind the markdown structure"
    );
    let mut blocks_out = Vec::new();
    for page_blocks in &pages {
        let left = page_left(page_blocks.iter().flatten());
        for lines in page_blocks {
            if let Some(markdown) = render_block(lines, &sizes, left, options) {
                blocks_out.push(markdown);
            }
        }
    }

    Ok(finish(blocks_out))
}

/// One MuPDF line as the heuristics see it, or `None` for blank ones.
fn extracted_line(line: &mupdf::StructuredTextLine) -> Option<Line> {
    let text = line.text.trim();
    if text.is_empty() {
        return None;
    }

    // Lines set at a single size report it on every character; a line of
    // mixed sizes is known by its largest, which is what the eye reads as
    // the line's size. Empty geometry falls back to the line's height.
    let size = line.chars.iter().map(|ch| ch.size).fold(0.0_f32, f32::max);
    let size = if size > 0.0 {
        size
    } else {
        line.bounds.y1 - line.bounds.y0
    };

    Some(Line {
        text: text.to_string(),
        size,
        x0: line.bounds.x0,
    })
}

/// The font-size clusters of a document: which size is body text and which
/// sizes are heading levels.
#[derive(Debug)]
struct SizeMap {
    /// The most common size, weighted by the amount of text set in it.
    body: f32,
    /// Heading sizes, largest first; index zero is level one.
    headings: Vec<f32>,
}

impl SizeMap {
    fn build<'a>(lines: impl Iterator<Item = &'a Line>) -> Self {
        let mut weights: Vec<(f32, usize)> = Vec::new();
        for line in lines {
            let bucket = bucket(line.size);
            let weight = line.text.chars().count().max(1);
            match weights.iter_mut().find(|(size, _)| *size == bucket) {
                Some(entry) => entry.1 += weight,
                None => weights.push((bucket, weight)),
            }
        }

        // Most text wins; a tie between equally heavy sizes is broken toward
        // the larger, which is the likelier body of a top-heavy page.
        let body = weights
            .iter()
            .max_by(|&&(size_a, weight_a), &(size_b, weight_b)| {
                weight_a.cmp(weight_b).then(size_a.total_cmp(size_b))
            })
            .map(|&(size, _)| size)
            .unwrap_or(12.0);

        let mut headings: Vec<f32> = weights
            .iter()
            .map(|&(size, _)| size)
            .filter(|&size| size > body * HEADING_MARGIN)
            .collect();
        headings.sort_by(|a, b| b.total_cmp(a));

        Self { body, headings }
    }

    /// The one-based heading level of `size`, or `None` for body text.
    fn level(&self, size: f32) -> Option<usize> {
        let bucket = bucket(size);
        self.headings
            .iter()
            .position(|&heading| heading == bucket)
            .map(|index| (index + 1).min(MAX_HEADING_LEVEL))
    }
}

/// A size bucketed to half points, so 11.98 and 12.02 are one size.
fn bucket(size: f32) -> f32 {
    (size * 2.0).round() / 2.0
}

/// The page's dominant left margin: the left edge its lines most often start
/// at, with ties going to the smaller edge.
///
/// Indentation is measured against this rather than against each block's own
/// edge, because an extractor is free to give an indented line a block of its
/// own, where its indent would otherwise disappear.
fn page_left<'a>(lines: impl Iterator<Item = &'a Line>) -> f32 {
    let mut weights: Vec<(f32, usize)> = Vec::new();
    for line in lines {
        let edge = line.x0.round();
        match weights
            .iter_mut()
            .find(|(edge_weighted, _)| *edge_weighted == edge)
        {
            Some(entry) => entry.1 += 1,
            None => weights.push((edge, 1)),
        }
    }

    weights
        .iter()
        .max_by(|&&(edge_a, count_a), &(edge_b, count_b)| {
            // Ties go to the smaller edge, so one deep-indented line cannot
            // drag the margin in after it.
            count_a.cmp(count_b).then(edge_b.total_cmp(&edge_a))
        })
        .map(|&(edge, _)| edge)
        .unwrap_or(0.0)
}

/// One Markdown construct within a block.
enum Element {
    Heading(usize, String),
    Item(String),
    Paragraph(String),
}

/// Turn one text block into Markdown, or `None` when it holds nothing.
///
/// `page_left` is the page's dominant left margin; see [`page_left`].
fn render_block(
    lines: &[Line],
    sizes: &SizeMap,
    page_left: f32,
    options: &MarkdownOptions,
) -> Option<String> {
    let mut elements: Vec<Element> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    // Where the line that started the last list item begins, so wrapped
    // tails hanging under it can be told from a fresh paragraph at the
    // block's left edge.
    let mut item_x0: Option<f32> = None;

    for line in lines {
        let heading = options.headings.then(|| sizes.level(line.size)).flatten();
        let bullet = options.lists.then(|| bullet_text(&line.text)).flatten();

        if let Some(level) = heading {
            flush(&mut elements, &mut paragraph);
            item_x0 = None;
            elements.push(Element::Heading(level, clean(&line.text)));
        } else if let Some(text) = bullet {
            flush(&mut elements, &mut paragraph);
            item_x0 = Some(line.x0);
            elements.push(Element::Item(text));
        } else if hanging_under_item(&elements, item_x0, line) {
            if let Some(Element::Item(item)) = elements.last_mut() {
                item.push(' ');
                item.push_str(&clean(&line.text));
            }
        } else if options.lists && line.x0 >= page_left + line.size {
            // Indented a full em from the page's left margin: a list item
            // whose producer drew an indent instead of a glyph.
            flush(&mut elements, &mut paragraph);
            item_x0 = Some(line.x0);
            elements.push(Element::Item(clean(&line.text)));
        } else {
            item_x0 = None;
            paragraph.push(clean(&line.text));
        }
    }
    flush(&mut elements, &mut paragraph);

    join_elements(elements)
}

/// Whether `line` is the wrapped tail of the list item above it: an item was
/// just emitted, and this line hangs at least half an em to the right of
/// where that item began.
fn hanging_under_item(elements: &[Element], item_x0: Option<f32>, line: &Line) -> bool {
    matches!(elements.last(), Some(Element::Item(_)))
        && item_x0.is_some_and(|x| line.x0 >= x + line.size * 0.5)
}

/// The item text when `text` starts with a bullet glyph, else `None`.
///
/// The glyph must be followed by a space, so a hyphenated first word is not
/// an item.
fn bullet_text(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if !BULLETS.contains(&first) {
        return None;
    }

    let rest = chars.as_str();
    let rest = rest
        .strip_prefix(' ')
        .or_else(|| rest.strip_prefix('\u{a0}'))?;
    let cleaned = clean(rest);
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Collapse whitespace runs, which extraction stretches around kerning.
fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Turn the accumulated paragraph lines into an element.
fn flush(elements: &mut Vec<Element>, paragraph: &mut Vec<String>) {
    if !paragraph.is_empty() {
        elements.push(Element::Paragraph(paragraph.join(" ")));
        paragraph.clear();
    }
}

/// Join the elements of one block: list items sit on consecutive lines,
/// everything else is separated by a blank line.
fn join_elements(elements: Vec<Element>) -> Option<String> {
    if elements.is_empty() {
        return None;
    }

    let mut out = String::new();
    let mut previous_was_item = false;
    for element in elements {
        let (text, is_item) = match element {
            Element::Heading(level, text) => (
                format!("{} {}", "#".repeat(level.min(MAX_HEADING_LEVEL)), text),
                false,
            ),
            Element::Item(text) => (format!("- {text}"), true),
            Element::Paragraph(text) => (text, false),
        };

        if !out.is_empty() {
            out.push_str(if is_item && previous_was_item {
                "\n"
            } else {
                "\n\n"
            });
        }
        out.push_str(&text);
        previous_was_item = is_item;
    }

    Some(out)
}

/// Join the blocks of the document into one document, ending in a newline.
///
/// Blocks that are list items running into each other — common when the
/// extractor gives every item a block of its own — join with a single
/// newline, so the list stays tight instead of turning loose.
fn finish(blocks: Vec<String>) -> String {
    if blocks.is_empty() {
        return String::new();
    }

    let is_item = |line: &str| line.starts_with("- ");

    let mut out = String::new();
    let mut previous_ended_with_item = false;
    for block in blocks {
        let starts_with_item = is_item(&block);
        if !out.is_empty() {
            out.push_str(if starts_with_item && previous_ended_with_item {
                "\n"
            } else {
                "\n\n"
            });
        }
        out.push_str(&block);
        previous_ended_with_item = block.lines().last().is_some_and(is_item);
    }

    out.push('\n');
    out
}

fn mupdf_error(path: &Path, source: mupdf::Error) -> PdfError {
    PdfError::Render {
        path: path.to_path_buf(),
        reason: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, size: f32, x0: f32) -> Line {
        Line {
            text: text.to_string(),
            size,
            x0,
        }
    }

    fn options() -> MarkdownOptions {
        MarkdownOptions::default()
    }

    /// The left margin most of the page's lines start at.
    const LEFT: f32 = 72.0;

    #[test]
    fn defaults_convert_everything_with_structure() {
        let options = MarkdownOptions::default();
        assert_eq!(options.pages, None);
        assert!(options.headings && options.lists);
    }

    #[test]
    fn the_heaviest_size_is_the_body() {
        let lines = [
            line("Title", 36.0, 72.0),
            line("Subtitle", 24.0, 72.0),
            line("Ordinary body text that goes on", 12.0, 72.0),
            line("More body text to outweigh the rest", 12.0, 72.0),
        ];
        let sizes = SizeMap::build(lines.iter());

        assert_eq!(sizes.body, 12.0);
        assert_eq!(sizes.headings, vec![36.0, 24.0]);
        assert_eq!(sizes.level(36.0), Some(1));
        assert_eq!(sizes.level(24.0), Some(2));
        assert_eq!(sizes.level(12.0), None);
    }

    #[test]
    fn near_body_sizes_are_not_headings() {
        let lines = [
            line("Slightly larger line here", 13.0, 72.0),
            line("Ordinary body text that goes on", 12.0, 72.0),
            line("More body text", 12.0, 72.0),
        ];
        let sizes = SizeMap::build(lines.iter());

        assert_eq!(sizes.body, 12.0);
        assert!(sizes.headings.is_empty(), "{:?}", sizes.headings);
    }

    #[test]
    fn levels_cap_at_six() {
        let lines = [
            line("Body text filling the page", 10.0, 72.0),
            line("A", 40.0, 72.0),
            line("B", 36.0, 72.0),
            line("C", 32.0, 72.0),
            line("D", 28.0, 72.0),
            line("E", 24.0, 72.0),
            line("F", 20.0, 72.0),
            line("G", 18.0, 72.0),
        ];
        let sizes = SizeMap::build(lines.iter());

        assert_eq!(sizes.level(40.0), Some(1));
        assert_eq!(sizes.level(18.0), Some(6));
    }

    #[test]
    fn sizes_bucket_to_half_points() {
        let lines = [
            line("Title", 35.98, 72.0),
            line("Body body body body body", 12.02, 72.0),
        ];
        let sizes = SizeMap::build(lines.iter());

        assert_eq!(sizes.body, 12.0);
        assert_eq!(sizes.level(36.02), Some(1));
    }

    #[test]
    fn bullet_glyphs_make_items() {
        assert_eq!(
            bullet_text("\u{2022} First item"),
            Some("First item".into())
        );
        assert_eq!(bullet_text("- Second item"), Some("Second item".into()));
        assert_eq!(bullet_text("\u{25E6} Third"), Some("Third".into()));
    }

    #[test]
    fn a_bullet_needs_something_after_it() {
        assert_eq!(bullet_text("\u{2022}"), None);
        assert_eq!(bullet_text("\u{2022}   "), None);
        assert_eq!(bullet_text("-"), None);
    }

    #[test]
    fn lines_without_bullets_are_not_items() {
        assert_eq!(bullet_text("Ordinary paragraph text"), None);
        assert_eq!(bullet_text("-hyphenated-word"), None);
        assert_eq!(bullet_text("1970 was the year"), None);
    }

    #[test]
    fn a_block_becomes_headings_paragraph_and_items() {
        let lines = [
            line("Big Title", 36.0, 72.0),
            line("Smaller Heading", 24.0, 72.0),
            line("A paragraph of body text.", 12.0, 72.0),
            line("\u{2022} first item", 12.0, 72.0),
            line("\u{2022} second item", 12.0, 72.0),
        ];
        let sizes = SizeMap::build(lines.iter());
        let markdown = render_block(&lines, &sizes, LEFT, &options()).unwrap();

        assert_eq!(
            markdown,
            "# Big Title\n\n## Smaller Heading\n\n\
             A paragraph of body text.\n\n\
             - first item\n- second item"
        );
    }

    #[test]
    fn a_line_hanging_under_an_item_joins_it() {
        let lines = [
            line("\u{2022} an item whose text wraps", 12.0, 72.0),
            line("onto a second line", 12.0, 72.0 + 12.0),
            line("then a fresh paragraph", 12.0, 72.0),
        ];
        let sizes = SizeMap::build(lines.iter());
        let markdown = render_block(&lines, &sizes, LEFT, &options()).unwrap();

        assert_eq!(
            markdown,
            "- an item whose text wraps onto a second line\n\nthen a fresh paragraph"
        );
    }

    #[test]
    fn a_paragraph_line_after_an_item_is_not_swallowed() {
        let lines = [
            line("\u{2022} an item", 12.0, 72.0),
            line("a paragraph at the block edge", 12.0, 72.0),
        ];
        let sizes = SizeMap::build(lines.iter());
        let markdown = render_block(&lines, &sizes, LEFT, &options()).unwrap();

        assert_eq!(markdown, "- an item\n\na paragraph at the block edge");
    }

    #[test]
    fn an_indented_line_becomes_an_item_without_a_glyph() {
        let lines = [
            line("A lead-in paragraph.", 12.0, 72.0),
            line("indented item text", 12.0, 72.0 + 14.0),
        ];
        let sizes = SizeMap::build(lines.iter());
        let markdown = render_block(&lines, &sizes, LEFT, &options()).unwrap();

        assert_eq!(markdown, "A lead-in paragraph.\n\n- indented item text");
    }

    #[test]
    fn detection_can_be_turned_off() {
        let lines = [
            line("Big Title", 36.0, 72.0),
            line("\u{2022} an item", 12.0, 72.0),
        ];

        let sizes = SizeMap::build(lines.iter());
        let plain = render_block(
            &lines,
            &sizes,
            LEFT,
            &MarkdownOptions {
                headings: false,
                lists: false,
                ..options()
            },
        )
        .unwrap();

        assert_eq!(plain, "Big Title \u{2022} an item");
    }

    #[test]
    fn the_page_margin_is_the_edge_most_lines_start_at() {
        let lines = [
            line("body line one", 12.0, 72.0),
            line("body line two", 12.0, 72.1),
            line("an outlier deep in the margin", 12.0, 300.0),
        ];
        assert_eq!(page_left(lines.iter()), 72.0);

        // Ties go to the smaller edge.
        let lines = [line("one", 12.0, 96.0), line("two", 12.0, 72.0)];
        assert_eq!(page_left(lines.iter()), 72.0);
    }

    #[test]
    fn whitespace_is_collapsed() {
        assert_eq!(clean("  a   b\t c  "), "a b c");
    }

    #[test]
    fn empty_input_produces_empty_output() {
        assert_eq!(finish(Vec::new()), "");
        assert_eq!(finish(vec!["one".into()]), "one\n");
        assert_eq!(finish(vec!["one".into(), "two".into()]), "one\n\ntwo\n");
    }

    #[test]
    fn consecutive_item_blocks_stay_a_tight_list() {
        assert_eq!(finish(vec!["- a".into(), "- b".into()]), "- a\n- b\n");
        // An item block next to a paragraph keeps its blank line.
        assert_eq!(finish(vec!["- a".into(), "text".into()]), "- a\n\ntext\n");
        assert_eq!(finish(vec!["text".into(), "- a".into()]), "text\n\n- a\n");
    }
}
