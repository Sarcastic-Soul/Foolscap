//! Markdown to PDF, without any additional dependencies.
//!
//! The input is reduced to styled plain text — headings, emphasis, links,
//! code spans and list markers are flattened — and laid out with
//! [`crate::convert::text::text_to_pdf`]. Deliberately simple: the result
//! reads like the document, it does not look like a rendered page.
//!
//! A fully faithful rendering would go through LibreOffice instead; see
//! [`crate::convert::html::html_to_pdf`] for the formats it accepts.

use std::path::Path;

use crate::convert::text::{text_to_pdf_with_progress, TextToPdfOptions};
use crate::error::{PdfError, Result};
use crate::progress::ProgressFn;

/// Convert Markdown `text` into a PDF.
pub fn markdown_to_pdf(text: &str, output: &Path, options: &TextToPdfOptions) -> Result<()> {
    markdown_to_pdf_with_progress(text, output, options, None)
}

/// [`markdown_to_pdf`], reporting progress as the text layer does.
pub fn markdown_to_pdf_with_progress(
    text: &str,
    output: &Path,
    options: &TextToPdfOptions,
    progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    let plain = markdown_to_text(text);

    let options = if options.title.is_none() {
        TextToPdfOptions {
            title: first_heading(text),
            ..options.clone()
        }
    } else {
        options.clone()
    };

    text_to_pdf_with_progress(&plain, output, &options, progress)
}

/// Read a Markdown file and convert it into a PDF.
pub fn markdown_file_to_pdf(input: &Path, output: &Path, options: &TextToPdfOptions) -> Result<()> {
    markdown_file_to_pdf_with_progress(input, output, options, None)
}

/// [`markdown_file_to_pdf`], reporting progress as the text layer does.
pub fn markdown_file_to_pdf_with_progress(
    input: &Path,
    output: &Path,
    options: &TextToPdfOptions,
    progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    if !input.exists() {
        return Err(PdfError::NotFound(input.to_path_buf()));
    }

    let text = std::fs::read_to_string(input).map_err(|source| PdfError::Io {
        path: input.to_path_buf(),
        source,
    })?;

    let options = if options.title.is_none() {
        let title = first_heading(&text).or_else(|| {
            input
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        });
        TextToPdfOptions {
            title,
            ..options.clone()
        }
    } else {
        options.clone()
    };

    markdown_to_pdf_with_progress(&text, output, &options, progress)
}

/// The first `#` heading, for use as the document title.
fn first_heading(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let stripped = line.trim_start();
        let level = stripped.chars().take_while(|c| *c == '#').count();
        if level == 0 || stripped.len() == level {
            return None;
        }
        // Require a space after the run of hashes, so that `#tag` is not a
        // heading.
        if !stripped[level..].starts_with(' ') {
            return None;
        }

        let heading = strip_inline(stripped[level..].trim().trim_end_matches('#').trim_end());
        (!heading.is_empty()).then_some(heading)
    })
}

/// Reduce Markdown to the plain text the text layer lays out.
///
/// Headings are separated from the body by a blank line so they paginate
/// cleanly; everything else is flattened in place.
pub(crate) fn markdown_to_text(text: &str) -> String {
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");

    // Each line is paired with whether it is verbatim code, so that blank
    // lines inside a fence are not collapsed below.
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut in_fence = false;

    for line in normalised.split('\n') {
        // Fenced code passes through verbatim; the fence markers themselves
        // are structure, not content.
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }

        if in_fence {
            out.push((line.to_string(), true));
            continue;
        }

        let trimmed = line.trim();

        // A line of dashes or equals signs directly under a paragraph line
        // underlines a setext heading; anywhere else a dash line is a rule.
        let under_text = out
            .last()
            .is_some_and(|(prev, code)| !*code && !prev.is_empty() && !prev.starts_with("- "));
        if under_text && is_setext_underline(trimmed) {
            if let Some((heading, _)) = out.pop() {
                out.push((String::new(), false));
                out.push((heading, false));
                out.push((String::new(), false));
            }
            continue;
        }

        match render_line(line) {
            Rendered::Heading(heading) => {
                out.push((String::new(), false));
                out.push((heading, false));
                out.push((String::new(), false));
            }
            Rendered::Line(rendered) => out.push((rendered, false)),
        }
    }

    // Collapse runs of blank lines outside code: Markdown uses one blank line
    // between blocks, and headings add their own spacing.
    let mut collapsed: Vec<String> = Vec::with_capacity(out.len());
    for (line, code) in out {
        if !code && line.is_empty() && collapsed.last().is_some_and(String::is_empty) {
            continue;
        }
        collapsed.push(line);
    }

    collapsed.join("\n")
}

fn is_setext_underline(line: &str) -> bool {
    !line.is_empty() && (line.chars().all(|c| c == '=') || line.chars().all(|c| c == '-'))
}

enum Rendered {
    Heading(String),
    Line(String),
}

/// Render one non-fence line to plain text.
fn render_line(line: &str) -> Rendered {
    let trimmed = line.trim_start();

    // ATX headings.
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes > 0 && trimmed[hashes..].starts_with(' ') {
        let heading = strip_inline(trimmed[hashes..].trim().trim_end_matches('#').trim_end());
        return if heading.is_empty() {
            Rendered::Line(String::new())
        } else {
            Rendered::Heading(heading)
        };
    }

    // Blockquotes lose the marker but keep the text.
    let mut content = trimmed;
    while let Some(rest) = content.strip_prefix('>') {
        content = rest.trim_start();
    }

    // Horizontal rules become a row of dashes. Checked before list items,
    // because `* * *` and `- - -` look like bullets.
    let bare: String = content.chars().filter(|c| !c.is_whitespace()).collect();
    if bare.len() >= 3 && ["*", "-", "_"].iter().any(|m| bare == m.repeat(bare.len())) {
        return Rendered::Line("- - -".to_string());
    }

    // Unordered and ordered list items keep their text with a simple bullet.
    if let Some(item) = unordered_item(content) {
        return Rendered::Line(format!("- {}", strip_inline(item)));
    }
    if let Some(item) = ordered_item(content) {
        return Rendered::Line(format!("- {}", strip_inline(item)));
    }

    Rendered::Line(strip_inline(content.trim_end()))
}

fn unordered_item(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* ").or_else(|| line.strip_prefix("+ ")))?;
    Some(rest)
}

fn ordered_item(line: &str) -> Option<&str> {
    let dot = line.find(". ")?;
    let (number, _) = line.split_at(dot);
    if !number.chars().all(|c| c.is_ascii_digit()) || number.is_empty() {
        return None;
    }
    Some(line[dot + 2..].trim_start())
}

/// Strip inline Markdown — emphasis, code spans, links and images — leaving
/// the readable text.
fn strip_inline(text: &str) -> String {
    // Images first: `![alt](src)` keeps the alt text.
    let text = replace_labelled(text, '!');
    // Links: `[label](target)` keeps the label.
    let text = replace_labelled(&text, '\0');

    // Double markers first, then single ones.
    let text = text.replace("**", "").replace("__", "").replace("~~", "");
    remove_single_markers(&text)
}

/// Drop `*`, `_` and backtick markers, keeping the ones that are really text:
/// the underscores in `snake_case` and an asterisk standing alone, as in
/// `2 * 3`.
fn remove_single_markers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());

    for (index, &c) in chars.iter().enumerate() {
        let before = index.checked_sub(1).map(|i| chars[i]);
        let after = chars.get(index + 1).copied();

        let keep = match c {
            '`' => false,
            '_' => {
                before.is_some_and(char::is_alphanumeric)
                    && after.is_some_and(char::is_alphanumeric)
            }
            '*' => before.is_none_or(char::is_whitespace) && after.is_none_or(char::is_whitespace),
            _ => true,
        };

        if keep {
            out.push(c);
        }
    }

    out
}

fn replace_labelled(text: &str, prefix: char) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        let at_label = if prefix == '\0' {
            chars[index] == '['
        } else {
            chars[index] == prefix
                && chars.get(index + 1) == Some(&'[')
                // An image's `!` must not be treated as emphasis elsewhere.
                && prefix == '!'
        };

        let start = if at_label && prefix != '\0' {
            index + 1
        } else if at_label {
            index
        } else {
            out.push(chars[index]);
            index += 1;
            continue;
        };

        // Find `](`, then the matching `)`.
        if chars[start] != '[' {
            out.push(chars[index]);
            index += 1;
            continue;
        }

        let Some(label_end) = chars[start..].iter().position(|c| *c == ']') else {
            out.push_str(&chars[index..].iter().collect::<String>());
            break;
        };
        let label_end = start + label_end;

        if chars.get(label_end + 1) != Some(&'(') {
            // A reference-style `[label]` or bare brackets: keep the inside.
            out.push_str(&chars[start + 1..label_end].iter().collect::<String>());
            index = label_end + 1;
            continue;
        }

        let Some(target_end) = chars[label_end + 1..].iter().position(|c| *c == ')') else {
            out.push_str(&chars[start + 1..label_end].iter().collect::<String>());
            index = label_end + 1;
            continue;
        };

        out.push_str(&chars[start + 1..label_end].iter().collect::<String>());
        index = label_end + 1 + target_end + 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_become_separated_text() {
        let plain = markdown_to_text("# Title\n\nSome body.");

        assert!(plain.contains("Title"));
        assert!(plain.contains("Some body."));
    }

    #[test]
    fn the_first_heading_is_the_title() {
        assert_eq!(
            first_heading("# Hello World\n\ntext"),
            Some("Hello World".to_string())
        );
        assert_eq!(first_heading("no headings here"), None);
        // `#tag` is not a heading.
        assert_eq!(first_heading("#tag stuff"), None);
    }

    #[test]
    fn emphasis_and_code_are_flattened() {
        assert_eq!(strip_inline("**bold** and *italic*"), "bold and italic");
        assert_eq!(strip_inline("`code` span"), "code span");
        assert_eq!(strip_inline("~~struck~~"), "struck");
    }

    #[test]
    fn links_keep_their_labels() {
        assert_eq!(strip_inline("[Foolscap](https://example.com)"), "Foolscap");
        assert_eq!(strip_inline("![a photo](photo.png)"), "a photo");
    }

    #[test]
    fn lists_become_bullets() {
        let plain = markdown_to_text("- one\n* two\n1. three\n> quoted");

        assert!(plain.contains("- one"));
        assert!(plain.contains("- two"));
        assert!(plain.contains("- three"));
        assert!(plain.contains("quoted"));
        assert!(!plain.contains('>'));
    }

    #[test]
    fn fenced_code_passes_through_verbatim() {
        let plain = markdown_to_text("```\n# not a heading\n```");

        assert!(plain.contains("# not a heading"));
    }

    #[test]
    fn setext_underline_markers_are_dropped() {
        let plain = markdown_to_text("Title\n=====\n\nBody");

        assert!(plain.contains("Title"));
        assert!(plain.contains("Body"));
        assert!(!plain.contains("====="));
    }

    #[test]
    fn a_dash_line_after_a_blank_is_a_rule() {
        let plain = markdown_to_text("above\n\n---\n\nbelow");

        assert!(plain.contains("- - -"));
        assert!(plain.contains("above") && plain.contains("below"));
    }

    #[test]
    fn a_dash_line_under_text_makes_a_heading_not_a_rule() {
        let plain = markdown_to_text("Title\n-----\nBody");

        assert!(!plain.contains("---"));
        assert!(plain.contains("Title"));
    }

    #[test]
    fn a_dash_line_under_a_list_item_is_a_rule() {
        let plain = markdown_to_text("- item\n---\nafter");

        assert!(plain.contains("- item"));
        assert!(plain.contains("- - -"));
    }

    #[test]
    fn snake_case_and_lone_asterisks_survive() {
        assert_eq!(strip_inline("call my_function now"), "call my_function now");
        assert_eq!(strip_inline("2 * 3"), "2 * 3");
        assert_eq!(strip_inline("_emphasis_ here"), "emphasis here");
    }

    #[test]
    fn headings_get_blank_lines_around_them() {
        let plain = markdown_to_text("text\n# Head\nmore");

        assert_eq!(plain, "text\n\nHead\n\nmore");
    }

    #[test]
    fn blank_lines_inside_code_are_kept() {
        let plain = markdown_to_text("```\na\n\n\nb\n```");

        assert_eq!(plain, "a\n\n\nb");
    }

    #[test]
    fn closing_hashes_on_a_heading_are_dropped() {
        assert_eq!(first_heading("## Title ##"), Some("Title".to_string()));
        assert!(!markdown_to_text("## Title ##").contains('#'));
    }
}
