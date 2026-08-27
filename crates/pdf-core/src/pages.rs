//! Page selection.
//!
//! Users type one-indexed, inclusive ranges (`1-3,7,9-`). Everything inside the
//! library is zero-indexed. The conversion happens here and nowhere else.

use std::fmt;
use std::str::FromStr;

use crate::error::{PdfError, Result};

/// A parsed page selection, kept in the form the user wrote it so that it can be
/// resolved against documents of different lengths.
///
/// Order and duplicates are preserved: `3,1,1` resolves to pages 3, 1, 1 in that
/// sequence. Operations that reorder or extract rely on this; operations that
/// only mark pages (rotate, for instance) should tolerate repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageRange {
    /// Every page in the document, in document order.
    All,
    /// An ordered list of segments, evaluated left to right.
    Segments(Vec<Segment>),
}

/// One comma-separated piece of a page range. All values are one-indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    /// A single page.
    Single(usize),
    /// An inclusive span. `start` may exceed `end`, meaning a reversed run.
    Span { start: usize, end: usize },
    /// `N-`: from page N to the last page of the document.
    From(usize),
}

impl PageRange {
    /// Parse a user-supplied range such as `1-3,7,9-`, `all`, or `-4`.
    ///
    /// Accepted forms, comma-separated and whitespace-tolerant:
    ///
    /// | Form  | Meaning                                    |
    /// |-------|--------------------------------------------|
    /// | `all` | every page (only valid on its own)         |
    /// | `N`   | page N                                     |
    /// | `N-M` | pages N through M inclusive; reversed if N > M |
    /// | `N-`  | page N through the last page               |
    /// | `-M`  | page 1 through M                           |
    pub fn parse(spec: &str) -> Result<Self> {
        let trimmed = spec.trim();

        if trimmed.is_empty() {
            return Err(PdfError::InvalidPageRange {
                spec: spec.to_string(),
                reason: "range is empty".into(),
            });
        }

        if trimmed.eq_ignore_ascii_case("all") {
            return Ok(PageRange::All);
        }

        let mut segments = Vec::new();
        for piece in trimmed.split(',') {
            segments.push(parse_segment(piece, spec)?);
        }

        Ok(PageRange::Segments(segments))
    }

    /// Resolve to zero-indexed page positions against a document of
    /// `page_count` pages.
    ///
    /// Preserves the order the user wrote, including duplicates, and rejects
    /// any page outside the document.
    pub fn resolve(&self, page_count: usize) -> Result<Vec<usize>> {
        match self {
            PageRange::All => Ok((0..page_count).collect()),
            PageRange::Segments(segments) => {
                let mut out = Vec::new();
                for segment in segments {
                    segment.expand_into(page_count, &mut out)?;
                }
                Ok(out)
            }
        }
    }

    /// Resolve, then drop duplicates while keeping first-seen order.
    ///
    /// Operations that mark pages rather than emitting them — rotate, delete —
    /// want this; extraction and reordering do not.
    pub fn resolve_unique(&self, page_count: usize) -> Result<Vec<usize>> {
        let mut seen = vec![false; page_count];
        let mut out = Vec::new();
        for index in self.resolve(page_count)? {
            if !seen[index] {
                seen[index] = true;
                out.push(index);
            }
        }
        Ok(out)
    }
}

impl Segment {
    fn expand_into(&self, page_count: usize, out: &mut Vec<usize>) -> Result<()> {
        match *self {
            Segment::Single(page) => out.push(to_zero_indexed(page, page_count)?),
            Segment::Span { start, end } => {
                let first = to_zero_indexed(start, page_count)?;
                let last = to_zero_indexed(end, page_count)?;
                if first <= last {
                    out.extend(first..=last);
                } else {
                    out.extend((last..=first).rev());
                }
            }
            Segment::From(start) => {
                if page_count == 0 {
                    return Err(PdfError::PageOutOfRange {
                        requested: start,
                        total: 0,
                    });
                }
                let first = to_zero_indexed(start, page_count)?;
                out.extend(first..page_count);
            }
        }
        Ok(())
    }
}

fn parse_segment(piece: &str, whole_spec: &str) -> Result<Segment> {
    let piece = piece.trim();
    let invalid = |reason: &str| PdfError::InvalidPageRange {
        spec: whole_spec.to_string(),
        reason: reason.to_string(),
    };

    if piece.is_empty() {
        return Err(invalid("empty segment between commas"));
    }

    match piece.split_once('-') {
        None => Ok(Segment::Single(parse_page_number(piece, whole_spec)?)),
        Some((before, after)) => {
            let before = before.trim();
            let after = after.trim();

            if after.contains('-') {
                return Err(invalid(&format!("{piece:?} has more than one dash")));
            }

            match (before.is_empty(), after.is_empty()) {
                (true, true) => Err(invalid("\"-\" needs a page number on at least one side")),
                (true, false) => Ok(Segment::Span {
                    start: 1,
                    end: parse_page_number(after, whole_spec)?,
                }),
                (false, true) => Ok(Segment::From(parse_page_number(before, whole_spec)?)),
                (false, false) => Ok(Segment::Span {
                    start: parse_page_number(before, whole_spec)?,
                    end: parse_page_number(after, whole_spec)?,
                }),
            }
        }
    }
}

fn parse_page_number(text: &str, whole_spec: &str) -> Result<usize> {
    let value: usize = text.parse().map_err(|_| PdfError::InvalidPageRange {
        spec: whole_spec.to_string(),
        reason: format!("{text:?} is not a page number"),
    })?;

    if value == 0 {
        return Err(PdfError::InvalidPageRange {
            spec: whole_spec.to_string(),
            reason: "page numbers start at 1".into(),
        });
    }

    Ok(value)
}

impl FromStr for PageRange {
    type Err = PdfError;

    fn from_str(s: &str) -> Result<Self> {
        PageRange::parse(s)
    }
}

impl fmt::Display for PageRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PageRange::All => f.write_str("all"),
            PageRange::Segments(segments) => {
                for (i, segment) in segments.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    match segment {
                        Segment::Single(page) => write!(f, "{page}")?,
                        Segment::Span { start, end } => write!(f, "{start}-{end}")?,
                        Segment::From(start) => write!(f, "{start}-")?,
                    }
                }
                Ok(())
            }
        }
    }
}

/// Convert a one-indexed page number to a zero-indexed position, bounds-checked
/// against `total`.
///
/// This is the single crossing point between what the user types and what the
/// library indexes with; every other module should go through it.
pub fn to_zero_indexed(requested: usize, total: usize) -> Result<usize> {
    if requested == 0 || requested > total {
        return Err(PdfError::PageOutOfRange { requested, total });
    }
    Ok(requested - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(spec: &str) -> PageRange {
        PageRange::parse(spec).expect("should parse")
    }

    fn resolve(spec: &str, page_count: usize) -> Vec<usize> {
        parse(spec).resolve(page_count).expect("should resolve")
    }

    #[test]
    fn zero_indexing_is_bounds_checked() {
        assert_eq!(to_zero_indexed(1, 3).unwrap(), 0);
        assert_eq!(to_zero_indexed(3, 3).unwrap(), 2);
        assert!(to_zero_indexed(0, 3).is_err());
        assert!(to_zero_indexed(4, 3).is_err());
        assert!(to_zero_indexed(1, 0).is_err());
    }

    #[test]
    fn all_is_case_insensitive() {
        assert_eq!(parse("all"), PageRange::All);
        assert_eq!(parse("ALL"), PageRange::All);
        assert_eq!(parse("  All  "), PageRange::All);
    }

    #[test]
    fn single_pages() {
        assert_eq!(parse("1"), PageRange::Segments(vec![Segment::Single(1)]));
        assert_eq!(
            parse("1,2,3"),
            PageRange::Segments(vec![
                Segment::Single(1),
                Segment::Single(2),
                Segment::Single(3)
            ])
        );
    }

    #[test]
    fn spans_and_open_ends() {
        assert_eq!(
            parse("1-3"),
            PageRange::Segments(vec![Segment::Span { start: 1, end: 3 }])
        );
        assert_eq!(parse("9-"), PageRange::Segments(vec![Segment::From(9)]));
        assert_eq!(
            parse("-4"),
            PageRange::Segments(vec![Segment::Span { start: 1, end: 4 }])
        );
    }

    #[test]
    fn mixed_spec_from_the_plan() {
        assert_eq!(
            parse("1-3,7,9-"),
            PageRange::Segments(vec![
                Segment::Span { start: 1, end: 3 },
                Segment::Single(7),
                Segment::From(9),
            ])
        );
    }

    #[test]
    fn whitespace_is_tolerated() {
        assert_eq!(parse(" 1 - 3 , 7 "), parse("1-3,7"));
    }

    #[test]
    fn resolution_is_zero_indexed_and_inclusive() {
        assert_eq!(resolve("1", 5), vec![0]);
        assert_eq!(resolve("1-3", 5), vec![0, 1, 2]);
        assert_eq!(resolve("5", 5), vec![4]);
        assert_eq!(resolve("all", 3), vec![0, 1, 2]);
    }

    #[test]
    fn open_ended_span_reaches_the_last_page() {
        assert_eq!(resolve("3-", 5), vec![2, 3, 4]);
        assert_eq!(resolve("5-", 5), vec![4]);
    }

    #[test]
    fn leading_dash_starts_at_page_one() {
        assert_eq!(resolve("-3", 5), vec![0, 1, 2]);
    }

    #[test]
    fn reversed_spans_reverse_the_output() {
        assert_eq!(resolve("3-1", 5), vec![2, 1, 0]);
    }

    #[test]
    fn order_and_duplicates_are_preserved() {
        assert_eq!(resolve("3,1,1", 5), vec![2, 0, 0]);
        assert_eq!(resolve("1-2,1-2", 5), vec![0, 1, 0, 1]);
    }

    #[test]
    fn resolve_unique_keeps_first_seen_order() {
        let range = parse("3,1,3,2,1");
        assert_eq!(range.resolve_unique(5).unwrap(), vec![2, 0, 1]);
    }

    #[test]
    fn all_on_an_empty_document_is_empty_not_an_error() {
        assert_eq!(PageRange::All.resolve(0).unwrap(), Vec::<usize>::new());
    }

    #[test]
    fn out_of_range_pages_are_rejected() {
        let err = parse("6").resolve(5).unwrap_err();
        assert!(matches!(
            err,
            PdfError::PageOutOfRange {
                requested: 6,
                total: 5
            }
        ));

        assert!(parse("1-6").resolve(5).is_err());
        assert!(parse("6-").resolve(5).is_err());
        assert!(parse("1-").resolve(0).is_err());
    }

    #[test]
    fn page_zero_is_rejected_at_parse_time() {
        assert!(PageRange::parse("0").is_err());
        assert!(PageRange::parse("0-3").is_err());
        assert!(PageRange::parse("1-0").is_err());
    }

    #[test]
    fn malformed_specs_are_rejected() {
        for spec in ["", "   ", ",", "1,,2", "1-2-3", "-", "abc", "1-abc", "1.5"] {
            assert!(
                PageRange::parse(spec).is_err(),
                "{spec:?} should have been rejected"
            );
        }
    }

    #[test]
    fn display_round_trips_through_parse() {
        for spec in ["all", "1", "1-3", "9-", "1-3,7,9-"] {
            let parsed = parse(spec);
            assert_eq!(parsed.to_string(), spec);
            assert_eq!(PageRange::parse(&parsed.to_string()).unwrap(), parsed);
        }
    }
}
