//! Redaction: opaque boxes over rectangles, with the covered content removed
//! from the file rather than just hidden.
//!
//! [`redact`] walks each selected page's content, and every Form XObject the
//! page reaches, tracking the graphics state so it knows where things land on
//! the page. Inside the box it:
//!
//! - **Splits text.** Each glyph's box is worked out from the text matrix, the
//!   font's widths and the current transformation. Glyphs that touch the box
//!   are cut out of the text-show operator; the ones around them stay where
//!   they were, held in place by a kerning adjustment equal to the removed
//!   glyph's advance. Text that does not touch the box is left alone.
//! - **Descends into Form XObjects**, applying each form's `/Matrix` and the
//!   `cm` it is drawn under, to any depth up to [`MAX_FORM_DEPTH`]. A form that
//!   needs editing is copied for that one use, with its own resources, so a
//!   form shared with other pages or other places on this page is not changed
//!   for them.
//! - **Blanks image pixels.** An image that overlaps the box is decoded, the
//!   covered pixels are painted black, and the result replaces the image for
//!   that one use. The original is dropped from the file when nothing else
//!   uses it. An image this code cannot decode (CCITT, JBIG2, JPX, CMYK) cannot
//!   be blanked, so it is removed from the page instead of being left
//!   recoverable under the box.
//! - **Removes vector paths** that lie wholly inside the box, along with the
//!   path data.
//! - **Cuts off runaway forms.** A form nested deeper than [`MAX_FORM_DEPTH`],
//!   or one that draws itself, is removed from the page rather than followed.
//!
//! The original content streams are replaced and the document is pruned, so
//! the removed bytes do not survive as unreferenced objects. Pages lose their
//! `/Thumb` thumbnail, which would otherwise show the unredacted page.
//!
//! [`redact_find`] removes the text runs whose decoded string contains the
//! needle, on the page and in its forms, and draws no box.
//!
//! What is still not covered:
//!
//! - Annotations (including form-field values) and their appearance streams.
//! - Paths that only partly overlap the box; the box hides them, but their
//!   data remains.
//! - Inline images, shadings and pattern fills.
//! - Glyph widths for fonts without a `/Widths` entry or a built-in table are
//!   estimated, and Type0 fonts are assumed to use two-byte codes. Where widths
//!   are estimated the glyph boxes are padded sideways, but text close to the
//!   box edge can still be cut slightly wrong.
//! - Document metadata, outlines, and anything else outside page content.

use std::collections::HashMap;
use std::rc::Rc;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream};

use super::crop::Rect;
use crate::assemble::materialize_inherited_attributes;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

/// How many levels of Form XObject are searched. Deeper than this is removed;
/// real documents nest a handful at most.
pub const MAX_FORM_DEPTH: usize = 12;

/// What a redaction pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactReport {
    /// Pages the pass touched.
    pub pages_redacted: usize,
    /// Text-showing runs removed entirely from the content streams.
    pub runs_removed: usize,
    /// Opaque boxes drawn.
    pub boxes_drawn: usize,
    /// Text runs that had some glyphs cut out and the rest kept.
    pub runs_trimmed: usize,
    /// Individual glyphs removed, across whole and trimmed runs.
    pub glyphs_removed: usize,
    /// Form XObjects copied and edited.
    pub forms_edited: usize,
    /// Images whose covered pixels were painted over in the image data.
    pub images_blanked: usize,
    /// Images that could not be decoded and were removed from the page.
    pub images_removed: usize,
    /// Vector paths removed because they lay wholly inside the box.
    pub paths_removed: usize,
}

/// Cover `rect` on every selected page with an opaque black box, and remove
/// the text, image pixels and small paths under it.
///
/// See the module notes for exactly what is removed and what is not.
pub fn redact(doc: &mut Document, pages: &PageRange, rect: Rect) -> Result<RedactReport> {
    redact_with_progress(doc, pages, rect, None)
}

/// [`redact`], reporting one progress tick per selected page.
pub fn redact_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    rect: Rect,
    progress: Option<ProgressFn<'_>>,
) -> Result<RedactReport> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(PdfError::InvalidCropBox {
            reason: "the redaction box must have a positive width and height".into(),
        });
    }
    run(doc, pages, Mode::Area(rect), progress, "redacting page")
}

/// Remove the text runs containing `needle` on the selected pages.
///
/// Matching is a case-sensitive substring over each run's decoded bytes. A
/// matching run loses all its glyphs (the text after it keeps its place); no
/// box is drawn, since there are no glyph coordinates to place one honestly.
/// Runs inside Form XObjects are searched too.
pub fn redact_find(doc: &mut Document, pages: &PageRange, needle: &str) -> Result<RedactReport> {
    redact_find_with_progress(doc, pages, needle, None)
}

/// [`redact_find`], reporting one progress tick per selected page.
pub fn redact_find_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    needle: &str,
    progress: Option<ProgressFn<'_>>,
) -> Result<RedactReport> {
    if needle.is_empty() {
        return Err(PdfError::EmptySearchText);
    }

    let report = run(doc, pages, Mode::Find(needle), progress, "searching page")?;
    if report.runs_removed == 0 {
        return Err(PdfError::TextNotFound(needle.to_string()));
    }
    Ok(report)
}

#[derive(Clone, Copy)]
enum Mode<'a> {
    Area(Rect),
    Find(&'a str),
}

fn run(
    doc: &mut Document,
    pages: &PageRange,
    mode: Mode<'_>,
    mut progress: Option<ProgressFn<'_>>,
    message: &str,
) -> Result<RedactReport> {
    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    materialize_inherited_attributes(&mut doc.inner)?;
    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();

    let mut redactor = Redactor {
        doc: &mut doc.inner,
        mode,
        report: RedactReport::default(),
        fonts: HashMap::new(),
        names: 0,
        fallback: Rc::new(FontInfo::fallback()),
    };
    let mut edited = false;

    for (index, position) in selected.iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(selected.len()),
                format!("{message} {}", position + 1),
            ));
        }
        let before = redactor.report.runs_removed + redactor.report.runs_trimmed;
        let changed = redactor.page(page_ids[*position])?;
        edited |= changed;

        match mode {
            Mode::Area(_) => {
                redactor.report.pages_redacted += 1;
                redactor.report.boxes_drawn += 1;
            }
            Mode::Find(_) => {
                if redactor.report.runs_removed + redactor.report.runs_trimmed > before {
                    redactor.report.pages_redacted += 1;
                }
            }
        }
    }

    let report = redactor.report;
    if edited {
        // The replaced content streams, forms and images are now unreferenced
        // but still in the file; drop them so the removed bytes are gone.
        doc.inner.prune_objects();
    }
    Ok(report)
}

// ------------------------------------------------------------------ matrices

type Matrix = [f64; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` followed by `b`, in PDF's row-vector convention.
fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn translation(tx: f64, ty: f64) -> Matrix {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

fn apply(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
}

fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)])
}

/// Bounding box `[x0, y0, x1, y1]` of a rectangle's four corners after `m`.
fn transformed_bounds(m: &Matrix, x0: f64, y0: f64, x1: f64, y1: f64) -> [f64; 4] {
    let mut bounds = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
        let (px, py) = apply(m, x, y);
        bounds[0] = bounds[0].min(px);
        bounds[1] = bounds[1].min(py);
        bounds[2] = bounds[2].max(px);
        bounds[3] = bounds[3].max(py);
    }
    bounds
}

fn rect_bounds(rect: &Rect) -> [f64; 4] {
    [
        f64::from(rect.x0),
        f64::from(rect.y0),
        f64::from(rect.x1),
        f64::from(rect.y1),
    ]
}

/// Whether two boxes share area. Boxes that only touch do not overlap.
fn overlaps(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
}

fn contains(outer: &[f64; 4], inner: &[f64; 4]) -> bool {
    inner[0] >= outer[0] && inner[1] >= outer[1] && inner[2] <= outer[2] && inner[3] <= outer[3]
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn matrix_from(object: Option<&Object>) -> Option<Matrix> {
    let items = object?.as_array().ok()?;
    if items.len() != 6 {
        return None;
    }
    let mut m = IDENTITY;
    for (slot, item) in m.iter_mut().zip(items) {
        *slot = number(item)?;
    }
    Some(m)
}

fn operand(operation: &Operation, index: usize) -> f64 {
    operation
        .operands
        .get(index)
        .and_then(number)
        .unwrap_or(0.0)
}

// --------------------------------------------------------------------- fonts

/// Widths for the standard fonts, in 1/1000 em, for codes 32 to 126.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];
const TIMES: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 333, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];

#[derive(Clone, Copy)]
enum Standard {
    Helvetica { bold: bool },
    Times { bold: bool },
    Courier,
}

impl Standard {
    fn from_name(base_font: &str) -> Option<Self> {
        let name = base_font.to_ascii_lowercase();
        let bold = name.contains("bold");
        if name.contains("courier") {
            Some(Self::Courier)
        } else if name.contains("times") {
            Some(Self::Times { bold })
        } else if name.contains("helvetica") || name.contains("arial") {
            Some(Self::Helvetica { bold })
        } else {
            None
        }
    }

    fn width(self, code: u32) -> f64 {
        let table = |table: &[u16; 95], bold: bool| {
            let base = match code {
                32..=126 => f64::from(table[(code - 32) as usize]),
                _ => 556.0,
            };
            // Bold faces are a little wider; a small overestimate is the safe
            // direction when the real table is not at hand.
            if bold {
                base * 1.08
            } else {
                base
            }
        };
        match self {
            Self::Courier => 600.0,
            Self::Helvetica { bold } => table(&HELVETICA, bold),
            Self::Times { bold } => table(&TIMES, bold),
        }
    }
}

enum Metrics {
    Simple {
        first: i64,
        widths: Vec<f64>,
        missing: f64,
        standard: Option<Standard>,
    },
    Wide {
        default: f64,
        widths: HashMap<u32, f64>,
    },
}

struct FontInfo {
    metrics: Metrics,
    /// Bytes per character code: 1, or 2 for composite fonts.
    code_len: usize,
    /// Multiplier from the font's own width units to 1/1000 em (Type3 only).
    scale: f64,
    ascent: f64,
    descent: f64,
    /// False when widths are guesses, in which case glyph boxes are padded.
    exact: bool,
}

impl FontInfo {
    /// Used when no font is selected or the font cannot be found.
    fn fallback() -> Self {
        Self {
            metrics: Metrics::Simple {
                first: 0,
                widths: Vec::new(),
                missing: 600.0,
                standard: None,
            },
            code_len: 1,
            scale: 1.0,
            ascent: 0.8,
            descent: -0.25,
            exact: false,
        }
    }

    /// Advance of `code` in 1/1000 em.
    fn width(&self, code: u32) -> f64 {
        let width = match &self.metrics {
            Metrics::Simple {
                first,
                widths,
                missing,
                standard,
            } => {
                let index = i64::from(code) - first;
                if index >= 0 && (index as usize) < widths.len() {
                    widths[index as usize]
                } else if let (Some(standard), true) = (standard, widths.is_empty()) {
                    standard.width(code)
                } else {
                    *missing
                }
            }
            Metrics::Wide { default, widths } => widths.get(&code).copied().unwrap_or(*default),
        };
        width * self.scale
    }
}

fn resolve_dict(doc: &lopdf::Document, object: &Object) -> Option<Dictionary> {
    match doc.dereference(object).ok()?.1 {
        Object::Dictionary(dict) => Some(dict.clone()),
        Object::Stream(stream) => Some(stream.dict.clone()),
        _ => None,
    }
}

fn resolve_numbers(doc: &lopdf::Document, object: Option<&Object>) -> Vec<f64> {
    let Some(object) = object else {
        return Vec::new();
    };
    match doc.dereference(object) {
        Ok((_, Object::Array(items))) => items
            .iter()
            .map(|item| {
                doc.dereference(item)
                    .ok()
                    .and_then(|(_, item)| number(item))
                    .unwrap_or(0.0)
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_font(doc: &lopdf::Document, font: &Dictionary) -> FontInfo {
    let subtype = font
        .get(b"Subtype")
        .and_then(Object::as_name)
        .unwrap_or(b"Type1");
    let base_font = font
        .get(b"BaseFont")
        .and_then(Object::as_name)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_default();

    let descriptor_of = |dict: &Dictionary| {
        dict.get(b"FontDescriptor")
            .ok()
            .and_then(|object| resolve_dict(doc, object))
    };
    let mut ascent = 0.75;
    let mut descent = -0.2;
    let mut missing = 0.0;
    let mut read_descriptor = |descriptor: &Dictionary| {
        let value = |key: &[u8]| descriptor.get(key).ok().and_then(number);
        if let Some(a) = value(b"Ascent") {
            ascent = (a / 1000.0).clamp(0.5, 1.0);
        }
        if let Some(d) = value(b"Descent") {
            descent = (-d.abs() / 1000.0).clamp(-0.4, -0.1);
        }
        if let Some(m) = value(b"MissingWidth") {
            missing = m;
        }
    };

    if subtype == b"Type0" {
        let descendant = font
            .get(b"DescendantFonts")
            .ok()
            .and_then(|object| doc.dereference(object).ok())
            .and_then(|(_, object)| object.as_array().ok())
            .and_then(|items| items.first())
            .and_then(|object| resolve_dict(doc, object));
        let mut default = 1000.0;
        let mut widths = HashMap::new();
        if let Some(descendant) = &descendant {
            if let Some(descriptor) = descriptor_of(descendant) {
                read_descriptor(&descriptor);
            }
            default = descendant
                .get(b"DW")
                .ok()
                .and_then(number)
                .unwrap_or(1000.0);
            let entries = resolve_numbers_raw(doc, descendant.get(b"W").ok());
            parse_cid_widths(&entries, &mut widths);
        }
        let identity = font
            .get(b"Encoding")
            .and_then(Object::as_name)
            .is_ok_and(|name| name.starts_with(b"Identity"));
        return FontInfo {
            metrics: Metrics::Wide { default, widths },
            code_len: 2,
            scale: 1.0,
            ascent,
            descent,
            exact: identity,
        };
    }

    if let Some(descriptor) = descriptor_of(font) {
        read_descriptor(&descriptor);
    }
    let first = font.get(b"FirstChar").and_then(Object::as_i64).unwrap_or(0);
    let widths = resolve_numbers(doc, font.get(b"Widths").ok());
    let standard = Standard::from_name(&base_font);
    let scale = if subtype == b"Type3" {
        // Type3 widths are in glyph space; /FontMatrix says how big that is.
        matrix_from(font.get(b"FontMatrix").ok()).map_or(1.0, |m| m[0] * 1000.0)
    } else {
        1.0
    };
    let exact = !widths.is_empty() || standard.is_some();
    if widths.is_empty() && standard.is_none() {
        missing = 600.0;
    }

    FontInfo {
        metrics: Metrics::Simple {
            first,
            widths,
            missing,
            standard,
        },
        code_len: 1,
        scale,
        ascent,
        descent,
        exact,
    }
}

/// Like [`resolve_numbers`] but keeps nested arrays flat with a marker, since
/// `/W` mixes numbers and arrays.
fn resolve_numbers_raw(doc: &lopdf::Document, object: Option<&Object>) -> Vec<Object> {
    match object.and_then(|object| doc.dereference(object).ok()) {
        Some((_, Object::Array(items))) => items
            .iter()
            .map(|item| match doc.dereference(item) {
                Ok((_, resolved)) => resolved.clone(),
                Err(_) => Object::Null,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Fill `widths` from a CIDFont `/W` array: `c [w1 w2 ...]` or `c1 c2 w`.
fn parse_cid_widths(entries: &[Object], widths: &mut HashMap<u32, f64>) {
    let mut index = 0;
    while index < entries.len() {
        let Some(first) = number(&entries[index]) else {
            index += 1;
            continue;
        };
        match entries.get(index + 1) {
            Some(Object::Array(list)) => {
                for (offset, width) in list.iter().enumerate() {
                    if let Some(width) = number(width) {
                        widths.insert(first as u32 + offset as u32, width);
                    }
                }
                index += 2;
            }
            Some(second) => {
                let (Some(last), Some(width)) =
                    (number(second), entries.get(index + 2).and_then(number))
                else {
                    index += 1;
                    continue;
                };
                // Bounded so a hostile range cannot allocate without limit.
                let last = (last as u32).min(first as u32 + 70_000);
                for code in first as u32..=last {
                    widths.insert(code, width);
                }
                index += 3;
            }
            None => break,
        }
    }
}

// ------------------------------------------------------------------ the walk

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    char_space: f64,
    word_space: f64,
    /// Horizontal scaling as a factor (`Tz` / 100).
    scale: f64,
    leading: f64,
    rise: f64,
    size: f64,
    font: Rc<FontInfo>,
    line_width: f64,
}

struct Walk {
    gs: GState,
    saved: Vec<GState>,
    tm: Matrix,
    tlm: Matrix,
}

impl Walk {
    fn move_line(&mut self, tx: f64, ty: f64) {
        self.tlm = multiply(&translation(tx, ty), &self.tlm);
        self.tm = self.tlm;
    }
}

enum Item {
    Text(Vec<u8>),
    Adjust(f64),
}

enum DoAction {
    Keep,
    Replace(Vec<u8>),
    Drop,
}

struct Redactor<'a> {
    doc: &'a mut lopdf::Document,
    mode: Mode<'a>,
    report: RedactReport,
    fonts: HashMap<ObjectId, Rc<FontInfo>>,
    names: usize,
    fallback: Rc<FontInfo>,
}

fn decode_error(source: lopdf::Error) -> PdfError {
    PdfError::Internal(format!("could not decode the page content: {source}"))
}

fn encode_error(source: lopdf::Error) -> PdfError {
    PdfError::Internal(format!("could not encode the page content: {source}"))
}

impl Redactor<'_> {
    /// Redact one page. Returns whether its content was rewritten.
    fn page(&mut self, page_id: ObjectId) -> Result<bool> {
        let content = self.doc.get_page_content(page_id);
        let decoded = Content::decode(&content).map_err(decode_error)?;

        let resources = self
            .doc
            .get_dictionary(page_id)
            .ok()
            .and_then(|page| page.get(b"Resources").ok())
            .and_then(|object| resolve_dict(self.doc, object))
            .unwrap_or_default();
        let mut xobjects = resources
            .get(b"XObject")
            .ok()
            .and_then(|object| resolve_dict(self.doc, object))
            .unwrap_or_default();

        let mut stack = Vec::new();
        let (mut operations, changed) = self.process(
            decoded.operations,
            &resources,
            &mut xobjects,
            IDENTITY,
            0,
            &mut stack,
        )?;

        if let Mode::Area(rect) = self.mode {
            // Whatever transformation the page left in force must not move the
            // box: close everything the content opened, plus our own `q`.
            let open = unclosed_saves(&operations);
            operations.insert(0, Operation::new("q", vec![]));
            for _ in 0..=open {
                operations.push(Operation::new("Q", vec![]));
            }
            push_box(&mut operations, rect);
        } else if !changed {
            return Ok(false);
        }

        let encoded = Content { operations }.encode().map_err(encode_error)?;
        super::contents::replace_content(self.doc, page_id, encoded)?;

        let page = self.doc.get_object_mut(page_id)?.as_dict_mut()?;
        if changed {
            let mut resources = resources;
            resources.set("XObject", Object::Dictionary(xobjects));
            page.set("Resources", Object::Dictionary(resources));
        }
        if matches!(self.mode, Mode::Area(_)) {
            // A thumbnail of the unredacted page would defeat the redaction.
            page.remove(b"Thumb");
        }
        Ok(true)
    }

    fn fresh_name(&mut self, xobjects: &Dictionary) -> Vec<u8> {
        loop {
            self.names += 1;
            let name = format!("FsRedact{}", self.names).into_bytes();
            if !xobjects.has(&name) {
                return name;
            }
        }
    }

    fn font(&mut self, resources: &Dictionary, name: &[u8]) -> Rc<FontInfo> {
        let entry = resources
            .get(b"Font")
            .ok()
            .and_then(|object| resolve_dict(self.doc, object))
            .and_then(|fonts| fonts.get(name).ok().cloned());
        let Some(entry) = entry else {
            return self.fallback.clone();
        };

        let id = entry.as_reference().ok();
        if let Some(cached) = id.and_then(|id| self.fonts.get(&id)) {
            return cached.clone();
        }
        let Some(dict) = resolve_dict(self.doc, &entry) else {
            return self.fallback.clone();
        };
        let info = Rc::new(parse_font(self.doc, &dict));
        if let Some(id) = id {
            self.fonts.insert(id, info.clone());
        }
        info
    }

    /// Walk `operations`, removing what the mode says to remove. Returns the
    /// new operations and whether anything changed (including a form or image
    /// replaced through `xobjects`).
    fn process(
        &mut self,
        operations: Vec<Operation>,
        resources: &Dictionary,
        xobjects: &mut Dictionary,
        base: Matrix,
        depth: usize,
        stack: &mut Vec<ObjectId>,
    ) -> Result<(Vec<Operation>, bool)> {
        let mut walk = Walk {
            gs: GState {
                ctm: base,
                char_space: 0.0,
                word_space: 0.0,
                scale: 1.0,
                leading: 0.0,
                rise: 0.0,
                size: 0.0,
                font: self.fallback.clone(),
                line_width: 1.0,
            },
            saved: Vec::new(),
            tm: IDENTITY,
            tlm: IDENTITY,
        };
        let mut out: Vec<Operation> = Vec::with_capacity(operations.len());
        let mut changed = false;
        // Resource names whose drawing was replaced or dropped.
        let mut replaced: Vec<Vec<u8>> = Vec::new();

        // Path under construction: where it starts in `out`, its bounds in
        // page space, and whether a clip is pending.
        let mut path_start: Option<usize> = None;
        let mut path_bounds: Option<[f64; 4]> = None;
        let mut clip_pending = false;

        for operation in operations {
            let op = operation.operator.as_str();
            match op {
                "q" => walk.saved.push(walk.gs.clone()),
                "Q" => {
                    if let Some(previous) = walk.saved.pop() {
                        walk.gs = previous;
                    }
                }
                "cm" => {
                    let m = [
                        operand(&operation, 0),
                        operand(&operation, 1),
                        operand(&operation, 2),
                        operand(&operation, 3),
                        operand(&operation, 4),
                        operand(&operation, 5),
                    ];
                    walk.gs.ctm = multiply(&m, &walk.gs.ctm);
                }
                "w" => walk.gs.line_width = operand(&operation, 0),
                "BT" => {
                    walk.tm = IDENTITY;
                    walk.tlm = IDENTITY;
                }
                "Tc" => walk.gs.char_space = operand(&operation, 0),
                "Tw" => walk.gs.word_space = operand(&operation, 0),
                "Tz" => walk.gs.scale = operand(&operation, 0) / 100.0,
                "TL" => walk.gs.leading = operand(&operation, 0),
                "Ts" => walk.gs.rise = operand(&operation, 0),
                "Tf" => {
                    if let Some(name) = operation.operands.first().and_then(|o| o.as_name().ok()) {
                        walk.gs.font = self.font(resources, name);
                    }
                    walk.gs.size = operand(&operation, 1);
                }
                "Td" => walk.move_line(operand(&operation, 0), operand(&operation, 1)),
                "TD" => {
                    walk.gs.leading = -operand(&operation, 1);
                    walk.move_line(operand(&operation, 0), operand(&operation, 1));
                }
                "Tm" => {
                    walk.tm = [
                        operand(&operation, 0),
                        operand(&operation, 1),
                        operand(&operation, 2),
                        operand(&operation, 3),
                        operand(&operation, 4),
                        operand(&operation, 5),
                    ];
                    walk.tlm = walk.tm;
                }
                "T*" => walk.move_line(0.0, -walk.gs.leading),
                "Tj" | "TJ" | "'" | "\"" => {
                    changed |= self.show(&mut walk, operation, &mut out);
                    continue;
                }
                "Do" => {
                    let name = operation
                        .operands
                        .first()
                        .and_then(|o| o.as_name().ok())
                        .map(<[u8]>::to_vec);
                    let action = match name {
                        Some(name) => {
                            self.invoke(resources, xobjects, &name, &walk.gs.ctm, depth, stack)?
                        }
                        None => DoAction::Keep,
                    };
                    let original = operation.operands.first().cloned();
                    match action {
                        DoAction::Keep => out.push(operation),
                        DoAction::Replace(name) => {
                            changed = true;
                            replaced.extend(
                                original.and_then(|o| o.as_name().ok().map(<[u8]>::to_vec)),
                            );
                            out.push(Operation::new("Do", vec![Object::Name(name)]));
                        }
                        DoAction::Drop => {
                            changed = true;
                            replaced.extend(
                                original.and_then(|o| o.as_name().ok().map(<[u8]>::to_vec)),
                            );
                        }
                    }
                    continue;
                }
                "m" | "l" | "c" | "v" | "y" | "re" => {
                    path_start.get_or_insert(out.len());
                    let points = path_points(&operation);
                    let bounds =
                        path_bounds.get_or_insert([f64::MAX, f64::MAX, f64::MIN, f64::MIN]);
                    for (x, y) in points {
                        let (px, py) = apply(&walk.gs.ctm, x, y);
                        bounds[0] = bounds[0].min(px);
                        bounds[1] = bounds[1].min(py);
                        bounds[2] = bounds[2].max(px);
                        bounds[3] = bounds[3].max(py);
                    }
                }
                "W" | "W*" => clip_pending = true,
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                    let painted = op != "n";
                    let stroked = matches!(op, "S" | "s" | "B" | "B*" | "b" | "b*");
                    if let (Mode::Area(rect), true, false, Some(start), Some(mut bounds)) =
                        (self.mode, painted, clip_pending, path_start, path_bounds)
                    {
                        if stroked {
                            let m = &walk.gs.ctm;
                            let scale = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
                            let grow = walk.gs.line_width.abs() * scale;
                            bounds = [
                                bounds[0] - grow,
                                bounds[1] - grow,
                                bounds[2] + grow,
                                bounds[3] + grow,
                            ];
                        }
                        if contains(&rect_bounds(&rect), &bounds) {
                            out.truncate(start);
                            self.report.paths_removed += 1;
                            changed = true;
                            path_start = None;
                            path_bounds = None;
                            continue;
                        }
                    }
                    path_start = None;
                    path_bounds = None;
                    clip_pending = false;
                }
                _ => {}
            }
            out.push(operation);
        }

        // A resource entry that nothing draws any more still keeps the object
        // it names (and the removed content in it) in the file.
        for name in replaced {
            let still_drawn = out.iter().any(|operation| {
                operation.operator == "Do"
                    && operation.operands.first().and_then(|o| o.as_name().ok()) == Some(&name[..])
            });
            if !still_drawn {
                xobjects.remove(&name);
            }
        }

        Ok((out, changed))
    }

    /// Handle one text-showing operator. Returns whether it was rewritten.
    fn show(&mut self, walk: &mut Walk, operation: Operation, out: &mut Vec<Operation>) -> bool {
        // What the operator does besides showing text.
        let mut prefix: Vec<Operation> = Vec::new();
        match operation.operator.as_str() {
            "'" => {
                walk.move_line(0.0, -walk.gs.leading);
                prefix.push(Operation::new("T*", vec![]));
            }
            "\"" => {
                walk.gs.word_space = operand(&operation, 0);
                walk.gs.char_space = operand(&operation, 1);
                walk.move_line(0.0, -walk.gs.leading);
                prefix.push(Operation::new(
                    "Tw",
                    vec![Object::Real(operand(&operation, 0) as f32)],
                ));
                prefix.push(Operation::new(
                    "Tc",
                    vec![Object::Real(operand(&operation, 1) as f32)],
                ));
                prefix.push(Operation::new("T*", vec![]));
            }
            _ => {}
        }

        let items = text_items(&operation);
        let force_all =
            matches!(self.mode, Mode::Find(needle) if operation_text(&operation).contains(needle));
        let area = match self.mode {
            Mode::Area(rect) => Some(rect_bounds(&rect)),
            Mode::Find(_) => None,
        };

        let gs = walk.gs.clone();
        let font = gs.font.clone();
        let mut rewritten: Vec<Object> = Vec::new();
        let (mut total, mut removed) = (0usize, 0usize);

        for item in items {
            match item {
                Item::Adjust(amount) => {
                    rewritten.push(Object::Real(amount as f32));
                    let tx = -amount / 1000.0 * gs.size * gs.scale;
                    walk.tm = multiply(&translation(tx, 0.0), &walk.tm);
                }
                Item::Text(bytes) => {
                    let mut kept: Vec<u8> = Vec::new();
                    let mut at = 0;
                    while at < bytes.len() {
                        let len = if font.code_len == 2 && at + 1 < bytes.len() {
                            2
                        } else {
                            1
                        };
                        let code = bytes[at..at + len]
                            .iter()
                            .fold(0u32, |acc, &byte| (acc << 8) | u32::from(byte));
                        let width = font.width(code);
                        let spacing = gs.char_space
                            + if len == 1 && code == 32 {
                                gs.word_space
                            } else {
                                0.0
                            };
                        let advance = (width / 1000.0 * gs.size + spacing) * gs.scale;

                        total += 1;
                        let hit = force_all
                            || area.is_some_and(|area| {
                                glyph_overlaps(&walk.tm, &gs, &font, width, &area)
                            });
                        if hit {
                            removed += 1;
                            if !kept.is_empty() {
                                rewritten.push(Object::string_literal(std::mem::take(&mut kept)));
                            }
                            if gs.size.abs() > 1e-9 {
                                // Hold the following text where it was.
                                let hold = -(width + spacing * 1000.0 / gs.size);
                                rewritten.push(Object::Real(hold as f32));
                            }
                        } else {
                            kept.extend_from_slice(&bytes[at..at + len]);
                        }
                        walk.tm = multiply(&translation(advance, 0.0), &walk.tm);
                        at += len;
                    }
                    if !kept.is_empty() {
                        rewritten.push(Object::string_literal(kept));
                    }
                }
            }
        }

        if removed == 0 {
            out.push(operation);
            return false;
        }

        self.report.glyphs_removed += removed;
        if removed == total {
            self.report.runs_removed += 1;
        } else {
            self.report.runs_trimmed += 1;
        }
        out.extend(prefix);
        if gs.size.abs() > 1e-9 && !rewritten.is_empty() {
            out.push(Operation::new("TJ", vec![Object::Array(rewritten)]));
        }
        true
    }

    /// Decide what to do with `name Do`: descend into a form, blank an image.
    fn invoke(
        &mut self,
        resources: &Dictionary,
        xobjects: &mut Dictionary,
        name: &[u8],
        ctm: &Matrix,
        depth: usize,
        stack: &mut Vec<ObjectId>,
    ) -> Result<DoAction> {
        let Some(id) = xobjects.get(name).ok().and_then(|o| o.as_reference().ok()) else {
            return Ok(DoAction::Keep);
        };
        let Ok(Object::Stream(stream)) = self.doc.get_object(id) else {
            return Ok(DoAction::Keep);
        };
        let stream = stream.clone();

        match stream.dict.get(b"Subtype").and_then(Object::as_name) {
            Ok(b"Form") => self.invoke_form(resources, xobjects, id, stream, ctm, depth, stack),
            Ok(b"Image") => self.invoke_image(xobjects, &stream, ctm),
            _ => Ok(DoAction::Keep),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke_form(
        &mut self,
        resources: &Dictionary,
        xobjects: &mut Dictionary,
        id: ObjectId,
        form: Stream,
        ctm: &Matrix,
        depth: usize,
        stack: &mut Vec<ObjectId>,
    ) -> Result<DoAction> {
        if depth >= MAX_FORM_DEPTH || stack.contains(&id) {
            // Left in place it would stay reachable, and unchecked, from the
            // edited copy of the form that holds it.
            tracing::warn!(?id, depth, "form too deep or circular; removed");
            return Ok(DoAction::Drop);
        }
        let Ok(content) = form.decompressed_content() else {
            tracing::warn!(?id, "form content could not be decoded; left unredacted");
            return Ok(DoAction::Keep);
        };
        let Ok(decoded) = Content::decode(&content) else {
            return Ok(DoAction::Keep);
        };

        // A form without its own resources uses the ones it is drawn with.
        let (form_resources, mut form_xobjects) = match form
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|object| resolve_dict(self.doc, object))
        {
            Some(own) => {
                let nested = own
                    .get(b"XObject")
                    .ok()
                    .and_then(|object| resolve_dict(self.doc, object))
                    .unwrap_or_default();
                (own, nested)
            }
            None => (resources.clone(), xobjects.clone()),
        };

        let matrix = matrix_from(form.dict.get(b"Matrix").ok()).unwrap_or(IDENTITY);
        stack.push(id);
        let result = self.process(
            decoded.operations,
            &form_resources,
            &mut form_xobjects,
            multiply(&matrix, ctm),
            depth + 1,
            stack,
        );
        stack.pop();
        let (operations, changed) = result?;
        if !changed {
            return Ok(DoAction::Keep);
        }

        let encoded = Content { operations }.encode().map_err(encode_error)?;
        let mut dict = form.dict.clone();
        dict.remove(b"Filter");
        dict.remove(b"DecodeParms");
        let mut new_resources = form_resources;
        new_resources.set("XObject", Object::Dictionary(form_xobjects));
        dict.set("Resources", Object::Dictionary(new_resources));
        let mut copy = Stream::new(dict, Vec::new());
        copy.set_plain_content(encoded);

        let new_id = self.doc.add_object(copy);
        let new_name = self.fresh_name(xobjects);
        xobjects.set(new_name.clone(), Object::Reference(new_id));
        self.report.forms_edited += 1;
        Ok(DoAction::Replace(new_name))
    }

    fn invoke_image(
        &mut self,
        xobjects: &mut Dictionary,
        image: &Stream,
        ctm: &Matrix,
    ) -> Result<DoAction> {
        let Mode::Area(rect) = self.mode else {
            return Ok(DoAction::Keep);
        };
        let dimension = |key: &[u8]| {
            image
                .dict
                .get(key)
                .and_then(Object::as_i64)
                .ok()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
        };
        let (Some(width), Some(height), Some(inverse)) =
            (dimension(b"Width"), dimension(b"Height"), invert(ctm))
        else {
            return Ok(DoAction::Keep);
        };
        let area = rect_bounds(&rect);

        // The unit square an image occupies, as the page sees it.
        if !overlaps(&transformed_bounds(ctm, 0.0, 0.0, 1.0, 1.0), &area) {
            return Ok(DoAction::Keep);
        }

        // The box in image space bounds the pixels worth testing.
        let unit = transformed_bounds(&inverse, area[0], area[1], area[2], area[3]);
        let (w, h) = (f64::from(width), f64::from(height));
        let x0 = (unit[0].clamp(0.0, 1.0) * w).floor() as u32;
        let x1 = ((unit[2].clamp(0.0, 1.0) * w).ceil() as u32).min(width);
        // Image rows run down from the top of the unit square.
        let y0 = ((1.0 - unit[3].clamp(0.0, 1.0)) * h).floor() as u32;
        let y1 = (((1.0 - unit[1].clamp(0.0, 1.0)) * h).ceil() as u32).min(height);

        let covered = |x: u32, y: u32| {
            let (u0, u1) = (f64::from(x) / w, f64::from(x + 1) / w);
            let (v0, v1) = (1.0 - f64::from(y + 1) / h, 1.0 - f64::from(y) / h);
            overlaps(&transformed_bounds(ctm, u0, v0, u1, v1), &area)
        };
        let any = (y0..y1).any(|y| (x0..x1).any(|x| covered(x, y)));
        if !any {
            return Ok(DoAction::Keep);
        }

        match super::image::blank_pixels(self.doc, image, (x0, y0, x1, y1), &covered) {
            Ok(blanked) => {
                let new_id = self.doc.add_object(blanked);
                let new_name = self.fresh_name(xobjects);
                xobjects.set(new_name.clone(), Object::Reference(new_id));
                self.report.images_blanked += 1;
                Ok(DoAction::Replace(new_name))
            }
            Err(reason) => {
                tracing::warn!(
                    ?reason,
                    "image under the redaction box cannot be edited; removed"
                );
                self.report.images_removed += 1;
                Ok(DoAction::Drop)
            }
        }
    }
}

/// Whether the glyph about to be drawn at `tm` touches `area`.
fn glyph_overlaps(tm: &Matrix, gs: &GState, font: &FontInfo, width: f64, area: &[f64; 4]) -> bool {
    let sideways = if font.exact { 0.0 } else { 0.1 * gs.size.abs() };
    // Glyph space to text space: scale by size and horizontal scaling, lift by
    // the rise.
    let x0 = -sideways * gs.scale;
    let x1 = (width / 1000.0 * gs.size + sideways) * gs.scale;
    let (ya, yb) = (
        font.descent * gs.size + gs.rise,
        font.ascent * gs.size + gs.rise,
    );
    let to_page = multiply(tm, &gs.ctm);
    let bounds = transformed_bounds(&to_page, x0.min(x1), ya.min(yb), x0.max(x1), ya.max(yb));
    overlaps(&bounds, area)
}

fn path_points(operation: &Operation) -> Vec<(f64, f64)> {
    let numbers: Vec<f64> = operation.operands.iter().filter_map(number).collect();
    match operation.operator.as_str() {
        "re" if numbers.len() == 4 => {
            let (x, y, w, h) = (numbers[0], numbers[1], numbers[2], numbers[3]);
            vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
        }
        _ => numbers
            .chunks_exact(2)
            .map(|pair| (pair[0], pair[1]))
            .collect(),
    }
}

/// The items a text-show operator draws: strings and kerning adjustments.
fn text_items(operation: &Operation) -> Vec<Item> {
    let string = |object: Option<&Object>| match object {
        Some(Object::String(bytes, _)) => vec![Item::Text(bytes.clone())],
        _ => Vec::new(),
    };
    match operation.operator.as_str() {
        "Tj" | "'" => string(operation.operands.first()),
        "\"" => string(operation.operands.get(2)),
        "TJ" => operation
            .operands
            .first()
            .and_then(|operand| operand.as_array().ok())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match item {
                        Object::String(bytes, _) => Some(Item::Text(bytes.clone())),
                        other => number(other).map(Item::Adjust),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The searchable text an operation draws, decoded lossily from its string
/// operands. Enough for a substring match, not a text extractor.
fn operation_text(operation: &Operation) -> String {
    text_items(operation)
        .into_iter()
        .filter_map(|item| match item {
            Item::Text(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Item::Adjust(_) => None,
        })
        .collect()
}

/// How many `q` operators are never matched by a `Q`.
fn unclosed_saves(operations: &[Operation]) -> usize {
    let mut depth = 0usize;
    for operation in operations {
        match operation.operator.as_str() {
            "q" => depth += 1,
            "Q" => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

/// Append an opaque black `re f` for `rect`, isolated in `q ... Q`.
fn push_box(operations: &mut Vec<Operation>, rect: Rect) {
    operations.push(Operation::new("q", vec![]));
    // Filling in device grey keeps one line and no resource lookup.
    operations.push(Operation::new("g", vec![Object::Integer(0)]));
    operations.push(Operation::new(
        "re",
        vec![
            Object::Real(rect.x0),
            Object::Real(rect.y0),
            Object::Real(rect.width()),
            Object::Real(rect.height()),
        ],
    ));
    operations.push(Operation::new("f", vec![]));
    operations.push(Operation::new("Q", vec![]));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_op(operator: &str, text: &str) -> Operation {
        Operation::new(
            operator,
            vec![Object::string_literal(text.as_bytes().to_vec())],
        )
    }

    #[test]
    fn tj_text_is_extracted() {
        assert_eq!(operation_text(&text_op("Tj", "hello")), "hello");
    }

    #[test]
    fn tj_array_text_is_joined() {
        let operation = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(b"he".to_vec()),
                Object::Integer(-40),
                Object::string_literal(b"llo".to_vec()),
            ])],
        );
        assert_eq!(operation_text(&operation), "hello");
    }

    #[test]
    fn the_quote_operator_reads_its_only_string() {
        assert_eq!(operation_text(&text_op("'", "line")), "line");
    }

    #[test]
    fn matrices_compose_in_pdf_order() {
        // Scale by 2, then move by 10: a point at 1 lands at 12.
        let m = multiply(&[2.0, 0.0, 0.0, 2.0, 0.0, 0.0], &translation(10.0, 0.0));
        assert_eq!(apply(&m, 1.0, 0.0), (12.0, 0.0));
    }

    #[test]
    fn a_matrix_inverts() {
        let m = [2.0, 0.5, -0.5, 3.0, 7.0, 9.0];
        let back = multiply(&m, &invert(&m).unwrap());
        for (got, want) in back.iter().zip(IDENTITY) {
            assert!((got - want).abs() < 1e-9, "{back:?}");
        }
    }

    #[test]
    fn a_flat_matrix_has_no_inverse() {
        assert!(invert(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn touching_boxes_do_not_overlap() {
        assert!(!overlaps(&[0.0, 0.0, 1.0, 1.0], &[1.0, 0.0, 2.0, 1.0]));
        assert!(overlaps(&[0.0, 0.0, 1.5, 1.0], &[1.0, 0.0, 2.0, 1.0]));
    }

    #[test]
    fn standard_widths_match_the_published_metrics() {
        let font = Standard::Helvetica { bold: false };
        assert_eq!(font.width(u32::from(b'H')), 722.0);
        assert_eq!(Standard::Courier.width(65), 600.0);
        assert_eq!(
            Standard::Times { bold: false }.width(u32::from(b'a')),
            444.0
        );
    }

    #[test]
    fn cid_widths_read_both_forms() {
        let mut widths = HashMap::new();
        parse_cid_widths(
            &[
                Object::Integer(1),
                Object::Array(vec![Object::Integer(500), Object::Integer(600)]),
                Object::Integer(10),
                Object::Integer(12),
                Object::Integer(250),
            ],
            &mut widths,
        );
        assert_eq!(widths[&1], 500.0);
        assert_eq!(widths[&2], 600.0);
        assert_eq!(widths[&11], 250.0);
    }
}
