//! Page labels: the numbers a viewer shows for each page, which need not match
//! the physical page index. A front matter in roman numerals followed by
//! arabic numbers is the usual case.
//!
//! The labels are the `/PageLabels /Nums` number tree in the catalog: pairs of
//! a zero-based starting page index and a dictionary holding the style `/S`,
//! the prefix `/P`, and the first number `/St`.

use lopdf::{Dictionary, Object, StringFormat};

use crate::document::Document;
use crate::error::{PdfError, Result};

/// The numbering style of a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    Decimal,
    UpperRoman,
    LowerRoman,
    UpperAlpha,
    LowerAlpha,
    /// Only the prefix, with no number.
    None,
}

impl LabelStyle {
    /// Parse a style name: `decimal`, `upper-roman`, `lower-roman`,
    /// `upper-alpha`, `lower-alpha` or `none`.
    pub fn parse(spec: &str) -> Option<Self> {
        Some(match spec {
            "decimal" => LabelStyle::Decimal,
            "upper-roman" => LabelStyle::UpperRoman,
            "lower-roman" => LabelStyle::LowerRoman,
            "upper-alpha" => LabelStyle::UpperAlpha,
            "lower-alpha" => LabelStyle::LowerAlpha,
            "none" => LabelStyle::None,
            _ => return None,
        })
    }

    /// The `/S` name in the file, or `None` for a prefix-only range.
    fn pdf_name(self) -> Option<&'static [u8]> {
        match self {
            LabelStyle::Decimal => Some(b"D"),
            LabelStyle::UpperRoman => Some(b"R"),
            LabelStyle::LowerRoman => Some(b"r"),
            LabelStyle::UpperAlpha => Some(b"A"),
            LabelStyle::LowerAlpha => Some(b"a"),
            LabelStyle::None => None,
        }
    }

    fn from_pdf_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"D" => LabelStyle::Decimal,
            b"R" => LabelStyle::UpperRoman,
            b"r" => LabelStyle::LowerRoman,
            b"A" => LabelStyle::UpperAlpha,
            b"a" => LabelStyle::LowerAlpha,
            _ => return None,
        })
    }
}

/// One numbered run of pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelRange {
    /// The first page of the range, one-indexed.
    pub start_page: usize,
    pub style: LabelStyle,
    /// Text before the number, such as `A-`.
    pub prefix: String,
    /// The number of the first page in the range, from 1.
    pub first: u32,
}

/// The label shown for every page, in page order.
pub fn page_labels(doc: &Document) -> Result<Vec<String>> {
    let ranges = read_ranges(&doc.inner);
    let pages = doc.page_count();

    Ok((0..pages)
        .map(|index| {
            // The governing range is the last one that starts at or before this page.
            let governing = ranges.iter().rev().find(|(start, _)| *start <= index);
            match governing {
                Some((start, range)) => {
                    let value = range.first + (index - start) as u32;
                    format!("{}{}", range.prefix, format_number(range.style, value))
                }
                None => (index + 1).to_string(),
            }
        })
        .collect())
}

/// Replace the page labels with `ranges`. An empty list removes them.
///
/// Ranges must start on pages from 1 upward, in increasing order, with no
/// page starting twice.
pub fn set_page_labels(doc: &mut Document, ranges: &[LabelRange]) -> Result<()> {
    let mut previous = 0;
    for range in ranges {
        if range.start_page == 0 {
            return Err(PdfError::InvalidPageLabel(
                "pages are numbered from 1".into(),
            ));
        }
        if range.start_page <= previous {
            return Err(PdfError::InvalidPageLabel(format!(
                "range starting at page {} must come after the one before it",
                range.start_page
            )));
        }
        if range.first == 0 {
            return Err(PdfError::InvalidPageLabel(
                "the first number of a range is at least 1".into(),
            ));
        }
        previous = range.start_page;
    }

    if ranges.is_empty() {
        doc.inner.catalog_mut()?.remove(b"PageLabels");
        return Ok(());
    }

    let mut nums = Vec::with_capacity(ranges.len() * 2);
    for range in ranges {
        let mut dict = Dictionary::new();
        if let Some(name) = range.style.pdf_name() {
            dict.set("S", Object::Name(name.to_vec()));
        }
        if !range.prefix.is_empty() {
            dict.set(
                "P",
                Object::String(range.prefix.as_bytes().to_vec(), StringFormat::Literal),
            );
        }
        dict.set("St", Object::Integer(i64::from(range.first)));
        nums.push(Object::Integer(range.start_page as i64 - 1));
        nums.push(Object::Dictionary(dict));
    }

    let mut labels = Dictionary::new();
    labels.set("Nums", Object::Array(nums));
    doc.inner
        .catalog_mut()?
        .set("PageLabels", Object::Dictionary(labels));
    Ok(())
}

/// The ranges as written in the file, sorted by starting page index (zero-based).
fn read_ranges(inner: &lopdf::Document) -> Vec<(usize, LabelRange)> {
    let Some(labels) = inner
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"PageLabels").ok())
        .and_then(|object| inner.dereference(object).ok())
        .and_then(|(_, object)| object.as_dict().ok())
    else {
        return Vec::new();
    };
    let Some(nums) = labels
        .get(b"Nums")
        .ok()
        .and_then(|object| inner.dereference(object).ok())
        .and_then(|(_, object)| object.as_array().ok())
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for pair in nums.chunks(2) {
        let [index, value] = pair else { continue };
        let Ok(index) = index.as_i64() else { continue };
        let Some(dict) = inner
            .dereference(value)
            .ok()
            .and_then(|(_, object)| object.as_dict().ok())
        else {
            continue;
        };
        let style = dict
            .get(b"S")
            .ok()
            .and_then(|object| object.as_name().ok())
            .and_then(LabelStyle::from_pdf_name)
            .unwrap_or(LabelStyle::None);
        let prefix = match dict.get(b"P").ok() {
            Some(Object::String(bytes, _)) => String::from_utf8_lossy(bytes).into_owned(),
            _ => String::new(),
        };
        let first = dict
            .get(b"St")
            .ok()
            .and_then(|object| object.as_i64().ok())
            .filter(|value| *value >= 1)
            .unwrap_or(1) as u32;
        out.push((
            index.max(0) as usize,
            LabelRange {
                start_page: index.max(0) as usize + 1,
                style,
                prefix,
                first,
            },
        ));
    }
    out.sort_by_key(|(start, _)| *start);
    out
}

/// The number as a style writes it. `None` writes nothing.
pub fn format_number(style: LabelStyle, value: u32) -> String {
    match style {
        LabelStyle::Decimal => value.to_string(),
        LabelStyle::UpperRoman => roman(value).to_uppercase(),
        LabelStyle::LowerRoman => roman(value),
        LabelStyle::UpperAlpha => alpha(value).to_uppercase(),
        LabelStyle::LowerAlpha => alpha(value),
        LabelStyle::None => String::new(),
    }
}

/// Lower-case roman numerals. Zero has none, so it is written as nothing.
fn roman(mut value: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (unit, digits) in TABLE {
        while value >= unit {
            out.push_str(digits);
            value -= unit;
        }
    }
    out
}

/// Lower-case letters: 1 is `a`, 26 is `z`, 27 is `aa`, as PDF specifies.
fn alpha(value: u32) -> String {
    if value == 0 {
        return String::new();
    }
    let letter = char::from(b'a' + ((value - 1) % 26) as u8);
    let repeat = ((value - 1) / 26 + 1) as usize;
    std::iter::repeat_n(letter, repeat).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roman_numerals() {
        assert_eq!(roman(1), "i");
        assert_eq!(roman(4), "iv");
        assert_eq!(roman(9), "ix");
        assert_eq!(roman(14), "xiv");
        assert_eq!(roman(1994), "mcmxciv");
    }

    #[test]
    fn alphabetic_labels_repeat_after_z() {
        assert_eq!(alpha(1), "a");
        assert_eq!(alpha(26), "z");
        assert_eq!(alpha(27), "aa");
        assert_eq!(alpha(28), "bb");
        assert_eq!(format_number(LabelStyle::UpperAlpha, 27), "AA");
    }
}
