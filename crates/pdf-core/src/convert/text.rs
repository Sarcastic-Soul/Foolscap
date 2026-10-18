//! Plain text to PDF, using a built-in font.
//!
//! The output uses Helvetica, one of the fourteen fonts every PDF reader
//! carries, so nothing is embedded and the file stays small. Text is wrapped
//! to the page and paginated; what you get is a readable document, not a
//! typeset one.

use std::path::Path;

use printpdf::{BuiltinFont, Mm, Op, PdfDocument, PdfFontHandle, PdfPage, PdfSaveOptions, Pt};
use printpdf::{Point, TextItem};

use crate::convert::images::PageSize;
use crate::error::{PdfError, Result};
use crate::progress::{Progress, ProgressFn};

/// The default body size, in points.
pub const DEFAULT_FONT_SIZE: f32 = 11.0;

/// The default blank border on every side, in points. One inch, as a
/// typewriter would leave.
pub const DEFAULT_MARGIN: f32 = 72.0;

/// Options for [`text_to_pdf`].
#[derive(Debug, Clone, PartialEq)]
pub struct TextToPdfOptions {
    pub page_size: PageSize,
    /// Body text size in points.
    pub font_size: f32,
    /// Blank border in points, on all four sides.
    pub margin: f32,
    /// Document title. Falls back to the file stem when converting a file.
    pub title: Option<String>,
}

impl Default for TextToPdfOptions {
    fn default() -> Self {
        Self {
            page_size: PageSize::A4,
            font_size: DEFAULT_FONT_SIZE,
            margin: DEFAULT_MARGIN,
            title: None,
        }
    }
}

/// Lay `text` out into pages of lines, wrapped to the content width.
pub fn text_to_pdf(text: &str, output: &Path, options: &TextToPdfOptions) -> Result<()> {
    text_to_pdf_with_progress(text, output, options, None)
}

/// [`text_to_pdf`], reporting one progress tick per page written.
pub fn text_to_pdf_with_progress(
    text: &str,
    output: &Path,
    options: &TextToPdfOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    if !(options.font_size.is_finite() && options.font_size > 0.0) {
        return Err(PdfError::Internal(format!(
            "font size must be positive, got {}",
            options.font_size
        )));
    }

    let (page_width, page_height) = options.page_size.points().unwrap_or_else(|| {
        // Fit-to-image has no fixed page; fall back to A4 for text, which has
        // no image to fit.
        PageSize::A4.points().expect("A4 has a fixed size")
    });

    let lines = wrap_text(text, page_width, options);
    let lines_per_page = lines_per_page(page_height, options).max(1);
    let chunks: Vec<&[String]> = lines.chunks(lines_per_page).collect();
    // Even an empty file is a valid one-page document rather than an error.
    let total = chunks.len().max(1);

    let mut document = PdfDocument::new(options.title.as_deref().unwrap_or("Foolscap"));
    let mut pdf_pages = Vec::with_capacity(total);

    if chunks.is_empty() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(0, Some(total), "writing page 1 of 1"));
        }
        pdf_pages.push(text_page(&[], page_width, page_height, options));
    } else {
        for (index, chunk) in chunks.iter().enumerate() {
            if let Some(tick) = progress.as_mut() {
                tick(Progress::new(
                    index,
                    Some(total),
                    format!("writing page {} of {total}", index + 1),
                ));
            }

            pdf_pages.push(text_page(chunk, page_width, page_height, options));
        }
    }

    if let Some(tick) = progress.as_mut() {
        tick(Progress::new(total, Some(total), "writing output"));
    }

    let bytes = document
        .with_pages(pdf_pages)
        .save(&PdfSaveOptions::default(), &mut Vec::new());

    write_output(output, &bytes)
}

/// Read a text file and lay it out as a PDF.
pub fn text_file_to_pdf(input: &Path, output: &Path, options: &TextToPdfOptions) -> Result<()> {
    text_file_to_pdf_with_progress(input, output, options, None)
}

/// [`text_file_to_pdf`], reporting one progress tick per page written.
pub fn text_file_to_pdf_with_progress(
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
        let title = input
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned());
        TextToPdfOptions {
            title,
            ..options.clone()
        }
    } else {
        options.clone()
    };

    text_to_pdf_with_progress(&text, output, &options, progress)
}

fn text_page(
    chunk: &[String],
    page_width: f32,
    page_height: f32,
    options: &TextToPdfOptions,
) -> PdfPage {
    let margin = options.margin.max(0.0);
    let font = PdfFontHandle::Builtin(BuiltinFont::Helvetica);

    // The cursor counts up from the bottom, so start one font size below the
    // top margin and let the line height carry it down from there.
    let cursor = Point {
        x: Pt(margin),
        y: Pt(page_height - margin - options.font_size),
    };

    let mut ops = vec![
        Op::StartTextSection,
        Op::SetFont {
            font,
            size: Pt(options.font_size),
        },
        Op::SetLineHeight {
            lh: Pt(line_height(options)),
        },
        Op::SetTextCursor { pos: cursor },
    ];

    for line in chunk {
        // A line the source wrapped in non-text bytes could still carry a
        // control character; those have no glyph and would confuse the reader.
        let clean: String = line
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        let expanded = clean.replace('\t', "        ");
        ops.push(Op::ShowText {
            items: vec![TextItem::Text(if expanded.is_empty() {
                " ".to_string()
            } else {
                expanded
            })],
        });
        ops.push(Op::AddLineBreak);
    }

    ops.push(Op::EndTextSection);

    PdfPage::new(
        Mm(points_to_mm(page_width)),
        Mm(points_to_mm(page_height)),
        ops,
    )
}

fn line_height(options: &TextToPdfOptions) -> f32 {
    options.font_size * 1.2
}

fn lines_per_page(page_height: f32, options: &TextToPdfOptions) -> usize {
    let margin = options.margin.max(0.0);
    let content = (page_height - margin * 2.0).max(line_height(options));

    (content / line_height(options)).floor() as usize
}

/// How many characters fit on a content line.
///
/// Helvetica averages about half an em per glyph in running text; deliberately
/// conservative, so that an unusually wide line wraps early rather than
/// running past the margin.
fn wrap_width(page_width: f32, options: &TextToPdfOptions) -> usize {
    let margin = options.margin.max(0.0);
    let content = (page_width - margin * 2.0).max(1.0);
    let per_char = (options.font_size * 0.5).max(1.0);

    ((content / per_char).floor() as usize).max(10)
}

/// Split `text` into lines that fit the content width, preserving blank lines
/// between paragraphs.
pub(crate) fn wrap_text(text: &str, page_width: f32, options: &TextToPdfOptions) -> Vec<String> {
    let width = wrap_width(page_width, options);
    let mut lines = Vec::new();

    // Normalise line endings and expand tabs up front, so wrapping sees what
    // the page will.
    let normalised = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "        ");

    for paragraph in normalised.split('\n') {
        if paragraph.trim().is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_width = 0usize;
        for word in paragraph.split(' ') {
            if word.is_empty() {
                continue;
            }

            // A single word wider than the line is split hard; hyphenation
            // would be nicer, but a mid-word break stays readable.
            for piece in split_long_word(word, width) {
                // Widths count characters, not bytes, so accented and
                // non-Latin text wraps where it will actually fall.
                let piece_width = piece.chars().count();
                if current.is_empty() {
                    current.push_str(&piece);
                    current_width = piece_width;
                } else if current_width + 1 + piece_width <= width {
                    current.push(' ');
                    current.push_str(&piece);
                    current_width += 1 + piece_width;
                } else {
                    lines.push(std::mem::take(&mut current));
                    current.push_str(&piece);
                    current_width = piece_width;
                }
            }
        }

        if !current.is_empty() {
            lines.push(current);
        }
    }

    lines
}

fn split_long_word(word: &str, width: usize) -> Vec<String> {
    if word.chars().count() <= width {
        return vec![word.to_string()];
    }

    // Split on character boundaries, so multibyte text is not torn apart.
    let chars: Vec<char> = word.chars().collect();
    chars
        .chunks(width.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn points_to_mm(points: f32) -> f32 {
    points * 25.4 / 72.0
}

fn write_output(output: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| PdfError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    std::fs::write(output, bytes).map_err(|source| PdfError::Io {
        path: output.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_lines_pass_through_unchanged() {
        let options = TextToPdfOptions::default();
        let lines = wrap_text("hello\nworld", 595.0, &options);

        assert_eq!(lines, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn blank_lines_survive_wrapping() {
        let options = TextToPdfOptions::default();
        let lines = wrap_text("first\n\nsecond", 595.0, &options);

        assert_eq!(lines.len(), 3);
        assert!(lines[1].is_empty());
    }

    #[test]
    fn long_lines_wrap_at_word_boundaries() {
        let options = TextToPdfOptions {
            font_size: 12.0,
            margin: 72.0,
            ..Default::default()
        };
        // A4 content width at 12pt is about 75 characters. Use a string long
        // enough to force a wrap regardless of exact word boundaries.
        let lines = wrap_text(
            "the quick brown fox jumps over the lazy dog and keeps running on and on and on and on",
            595.0,
            &options,
        );

        assert!(lines.len() >= 2);
        for line in &lines {
            assert!(line.len() <= 80, "line too long: {line:?}");
        }
        assert!(lines.join(" ").contains("quick brown fox"));
    }

    #[test]
    fn an_overlong_word_is_split_hard() {
        let options = TextToPdfOptions::default();
        let word = "a".repeat(500);
        let lines = wrap_text(&word, 595.0, &options);

        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.len() <= wrap_width(595.0, &options));
        }
    }

    #[test]
    fn accented_text_wraps_by_characters_not_bytes() {
        let options = TextToPdfOptions::default();
        let width = wrap_width(595.0, &options);
        // Every word is two bytes per character; counting bytes would wrap
        // this onto two lines even though it fits in one.
        let text = vec!["é"; width / 2].join(" ");
        assert!(text.chars().count() <= width);

        assert_eq!(wrap_text(&text, 595.0, &options).len(), 1);
    }

    #[test]
    fn windows_line_endings_do_not_leave_stray_carriages() {
        let options = TextToPdfOptions::default();
        let lines = wrap_text("one\r\ntwo\rthree", 595.0, &options);

        assert_eq!(
            lines,
            vec!["one".to_string(), "two".to_string(), "three".to_string()]
        );
    }

    #[test]
    fn a_page_holds_a_sensible_number_of_lines() {
        let options = TextToPdfOptions::default();
        // A4 is 842pt; two 1-inch margins leave ~698pt for ~13pt lines.
        let count = lines_per_page(841.89, &options);

        assert!((40..=60).contains(&count), "got {count}");
    }

    #[test]
    fn an_absurd_margin_still_leaves_room_for_a_line() {
        let options = TextToPdfOptions {
            margin: 10_000.0,
            ..Default::default()
        };

        assert!(lines_per_page(841.89, &options) >= 1);
        assert!(wrap_width(595.0, &options) >= 10);
    }
}
