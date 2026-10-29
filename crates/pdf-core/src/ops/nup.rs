//! N-up and booklet imposition operations.
//!
//! N-up places multiple source pages onto each output page in a grid.
//! Booklet imposition reorders pages into signatures for duplex printing
//! and folding, then applies 2×1 n-up.

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream};

use crate::assemble::materialize_inherited_attributes;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::ops::crop::{Fit, Margins, PageSize};
use crate::progress::{Progress, ProgressFn};

/// Layout order when placing pages on an n-up grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageOrder {
    /// Fill rows left-to-right, top-to-bottom (standard reading order).
    #[default]
    RowMajor,
    /// Fill columns top-to-bottom, left-to-right.
    ColumnMajor,
}

impl std::str::FromStr for PageOrder {
    type Err = PdfError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "row" | "row-major" => Ok(PageOrder::RowMajor),
            "column" | "col" | "column-major" => Ok(PageOrder::ColumnMajor),
            other => Err(PdfError::InvalidStamp(format!(
                "unknown page order {other:?}; expected row or column"
            ))),
        }
    }
}

/// Binding edge for booklet duplex printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Binding {
    /// Short-edge binding (flip on short edge), for landscape sheets folded horizontally.
    #[default]
    ShortEdge,
    /// Long-edge binding (flip on long edge), for portrait sheets folded vertically.
    LongEdge,
}

impl std::str::FromStr for Binding {
    type Err = PdfError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().replace('-', "").as_str() {
            "short" | "shortedge" => Ok(Binding::ShortEdge),
            "long" | "longedge" => Ok(Binding::LongEdge),
            other => Err(PdfError::InvalidStamp(format!(
                "unknown binding {other:?}; expected short-edge or long-edge"
            ))),
        }
    }
}

/// Options for n-up imposition.
#[derive(Debug, Clone)]
pub struct NupOptions {
    /// Number of rows in the grid.
    pub rows: usize,
    /// Number of columns in the grid.
    pub cols: usize,
    /// Output paper size.
    pub paper_size: PageSize,
    /// Margins around the grid on the output page.
    pub margins: Margins,
    /// Gap between grid cells in points.
    pub gap: f32,
    /// Draw a border around each placed page.
    pub border: bool,
    /// Page order within the grid.
    pub page_order: PageOrder,
    /// Scale mode for source pages within each cell.
    pub fit: Fit,
}

impl Default for NupOptions {
    fn default() -> Self {
        Self {
            rows: 2,
            cols: 2,
            paper_size: PageSize::A4,
            margins: Margins::default(),
            gap: 0.0,
            border: false,
            page_order: PageOrder::RowMajor,
            fit: Fit::Contain,
        }
    }
}

impl NupOptions {
    /// Validate the options.
    pub fn validate(&self) -> Result<()> {
        if self.rows == 0 || self.cols == 0 {
            return Err(PdfError::InvalidStamp(
                "rows and cols must be at least 1".into(),
            ));
        }
        if self.rows > 10 || self.cols > 10 {
            return Err(PdfError::InvalidStamp(
                "rows and cols must not exceed 10".into(),
            ));
        }
        self.margins.validate()?;
        if self.gap < 0.0 || !self.gap.is_finite() {
            return Err(PdfError::InvalidStamp("gap must be non-negative".into()));
        }
        let Some((w, h)) = self.paper_size.points() else {
            return Err(PdfError::InvalidMediaBox {
                reason: "FitImage not supported for n-up output".into(),
            });
        };
        let usable_w = w - self.margins.left - self.margins.right;
        let usable_h = h - self.margins.top - self.margins.bottom;
        if usable_w <= 0.0 || usable_h <= 0.0 {
            return Err(PdfError::InvalidMediaBox {
                reason: "margins exceed paper size".into(),
            });
        }
        Ok(())
    }
}

/// Options for booklet imposition.
#[derive(Debug, Clone)]
pub struct BookletOptions {
    /// Pages per signature (must be a multiple of 4).
    pub signature_size: usize,
    /// Output paper size (the sheet size, not the content size).
    pub paper_size: PageSize,
    /// Binding edge for duplex printing.
    pub binding: Binding,
    /// Margins around the 2×1 grid on each sheet.
    pub margins: Margins,
    /// Gap between the two pages on a sheet.
    pub gap: f32,
    /// Draw a border around each placed page.
    pub border: bool,
    /// Scale mode for source pages within each half-sheet.
    pub fit: Fit,
}

impl Default for BookletOptions {
    fn default() -> Self {
        Self {
            signature_size: 8,
            paper_size: PageSize::A4,
            binding: Binding::ShortEdge,
            margins: Margins::default(),
            gap: 0.0,
            border: false,
            fit: Fit::Contain,
        }
    }
}

impl BookletOptions {
    /// Validate the options.
    pub fn validate(&self) -> Result<()> {
        if self.signature_size == 0 || self.signature_size % 4 != 0 {
            return Err(PdfError::InvalidStamp(
                "signature size must be a multiple of 4".into(),
            ));
        }
        if self.signature_size > 32 {
            return Err(PdfError::InvalidStamp(
                "signature size must not exceed 32".into(),
            ));
        }
        self.margins.validate()?;
        if self.gap < 0.0 || !self.gap.is_finite() {
            return Err(PdfError::InvalidStamp("gap must be non-negative".into()));
        }
        let Some((w, h)) = self.paper_size.points() else {
            return Err(PdfError::InvalidMediaBox {
                reason: "FitImage not supported for booklet output".into(),
            });
        };
        let usable_w = w - self.margins.left - self.margins.right;
        let usable_h = h - self.margins.top - self.margins.bottom;
        if usable_w <= 0.0 || usable_h <= 0.0 {
            return Err(PdfError::InvalidMediaBox {
                reason: "margins exceed paper size".into(),
            });
        }
        Ok(())
    }
}

/// Place multiple source pages onto output pages in a grid.
///
/// Returns a new document with the imposed pages.
pub fn nup(doc: &Document, options: NupOptions) -> Result<Document> {
    nup_with_progress(doc, options, None)
}

/// [`nup`], reporting progress per output page.
pub fn nup_with_progress(
    doc: &Document,
    options: NupOptions,
    progress: Option<ProgressFn<'_>>,
) -> Result<Document> {
    options.validate()?;

    let source_pages = doc.page_count();
    if source_pages == 0 {
        return Err(PdfError::EmptySelection);
    }

    let pages_per_sheet = options.rows * options.cols;
    let output_page_count = source_pages.div_ceil(pages_per_sheet);

    let (paper_w, paper_h) = options.paper_size.points().unwrap();

    // Materialize the source document to ensure all resources are accessible
    let mut source_doc = doc.inner.clone();
    materialize_inherited_attributes(&mut source_doc)?;

    // Create a new output document
    let mut output_doc = lopdf::Document::new();
    output_doc.version = source_doc.version.clone();

    // Set up page tree for output document
    let output_pages_id = output_doc.new_object_id();
    let mut output_page_refs = Vec::with_capacity(output_page_count);

    // Get source page IDs
    let source_page_ids: Vec<ObjectId> = source_doc.get_pages().into_values().collect();

    // Calculate cell dimensions
    let usable_w = paper_w - options.margins.left - options.margins.right;
    let usable_h = paper_h - options.margins.top - options.margins.bottom;
    let total_gap_w = options.gap * (options.cols - 1) as f32;
    let total_gap_h = options.gap * (options.rows - 1) as f32;
    let cell_w = (usable_w - total_gap_w) / options.cols as f32;
    let cell_h = (usable_h - total_gap_h) / options.rows as f32;

    // For each output page
    let mut progress = progress;
    for output_idx in 0..output_page_count {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                output_idx,
                Some(output_page_count),
                format!("imposing sheet {}", output_idx + 1),
            ));
        }

        // Create a Form XObject for each source page that will go on this sheet
        let mut form_ids = Vec::new();
        let mut form_bboxes = Vec::new();
        let mut form_resources = Vec::new();

        for cell_idx in 0..pages_per_sheet {
            let source_idx = output_idx * pages_per_sheet + cell_idx;
            if source_idx >= source_pages {
                form_ids.push(None);
                form_bboxes.push((0.0, 0.0));
                form_resources.push(None);
                continue;
            }

            let source_page_id = source_page_ids[source_idx];

            // Extract page as Form XObject
            let (form_id, bbox, resources) =
                extract_page_as_form(&mut source_doc, &mut output_doc, source_page_id)?;
            form_ids.push(Some(form_id));
            form_bboxes.push(bbox);
            form_resources.push(Some(resources));
        }

        // Create the output page content stream
        let mut operations = Vec::new();

        // Place each cell
        for cell_idx in 0..pages_per_sheet {
            if form_ids[cell_idx].is_none() {
                continue;
            }

            let form_id = form_ids[cell_idx].unwrap();
            let (src_w, src_h) = form_bboxes[cell_idx];
            let _form_resources = form_resources[cell_idx].as_ref().unwrap();

            // Determine grid position based on page_order
            let (row, col) = match options.page_order {
                PageOrder::RowMajor => (cell_idx / options.cols, cell_idx % options.cols),
                PageOrder::ColumnMajor => (cell_idx % options.rows, cell_idx / options.rows),
            };

            let x = options.margins.left + col as f32 * (cell_w + options.gap);
            let y = paper_h
                - options.margins.top
                - (row + 1) as f32 * cell_h
                - row as f32 * options.gap;

            // Calculate scale to fit the cell
            let (scale_x, scale_y, offset_x, offset_y) = match options.fit {
                Fit::Stretch => (cell_w / src_w, cell_h / src_h, x, y),
                Fit::Contain | Fit::Cover => {
                    let scale = match options.fit {
                        Fit::Contain => (cell_w / src_w).min(cell_h / src_h),
                        Fit::Cover => (cell_w / src_w).max(cell_h / src_h),
                        _ => unreachable!(),
                    };
                    let scaled_w = src_w * scale;
                    let scaled_h = src_h * scale;
                    let offset_x = x + (cell_w - scaled_w) / 2.0;
                    let offset_y = y + (cell_h - scaled_h) / 2.0;
                    (scale, scale, offset_x, offset_y)
                }
            };

            // Add the Form XObject placement operation
            // cm matrix: [scale_x, 0, 0, scale_y, offset_x, offset_y]
            operations.push(Operation::new("q", vec![]));
            operations.push(Operation::new(
                "cm",
                vec![
                    Object::Real(scale_x),
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Real(scale_y),
                    Object::Real(offset_x),
                    Object::Real(offset_y),
                ],
            ));
            operations.push(Operation::new(
                "Do",
                vec![Object::Name(format!("Fm{}", form_id.0).as_bytes().to_vec())],
            ));
            operations.push(Operation::new("Q", vec![]));

            // Draw border if requested
            if options.border {
                operations.push(Operation::new("q", vec![]));
                operations.push(Operation::new(
                    "RG",
                    vec![Object::Real(0.5), Object::Real(0.5), Object::Real(0.5)],
                ));
                operations.push(Operation::new("w", vec![Object::Real(0.5)]));
                operations.push(Operation::new(
                    "re",
                    vec![
                        Object::Real(x),
                        Object::Real(y),
                        Object::Real(cell_w),
                        Object::Real(cell_h),
                    ],
                ));
                operations.push(Operation::new("S", vec![]));
                operations.push(Operation::new("Q", vec![]));
            }
        }

        // Create content stream
        let content_stream = Content { operations }
            .encode()
            .map_err(|e| PdfError::Internal(format!("failed to encode content stream: {e}")))?;
        let content_id = output_doc.add_object(Stream::new(Dictionary::new(), content_stream));

        // Merge all form resources
        let mut merged_resources = Dictionary::new();
        let mut xobjects = Dictionary::new();
        let mut fonts = Dictionary::new();
        let mut ext_gstates = Dictionary::new();
        let mut procsets = Vec::new();

        for (cell_idx, form_res) in form_resources.iter().enumerate() {
            if let Some(res) = form_res {
                if let Some(form_id) = form_ids[cell_idx] {
                    xobjects.set(
                        format!("Fm{}", form_id.0).as_bytes().to_vec(),
                        Object::Reference(form_id),
                    );
                }
                // Merge fonts
                if let Ok(Object::Dictionary(f)) = res.get(b"Font") {
                    for (k, v) in f.iter() {
                        if fonts.get(k).is_err() {
                            fonts.set(k.clone(), v.clone());
                        }
                    }
                }
                // Merge ExtGState
                if let Ok(Object::Dictionary(g)) = res.get(b"ExtGState") {
                    for (k, v) in g.iter() {
                        if ext_gstates.get(k).is_err() {
                            ext_gstates.set(k.clone(), v.clone());
                        }
                    }
                }
                // Merge ProcSet
                if let Ok(Object::Array(p)) = res.get(b"ProcSet") {
                    for item in p {
                        if let Object::Name(name) = item {
                            if !procsets.contains(name) {
                                procsets.push(name.clone());
                            }
                        }
                    }
                }
            }
        }

        if !xobjects.is_empty() {
            merged_resources.set("XObject", Object::Dictionary(xobjects));
        }
        if !fonts.is_empty() {
            merged_resources.set("Font", Object::Dictionary(fonts));
        }
        if !ext_gstates.is_empty() {
            merged_resources.set("ExtGState", Object::Dictionary(ext_gstates));
        }
        if !procsets.is_empty() {
            let procset_array: Vec<Object> = procsets.into_iter().map(Object::Name).collect();
            merged_resources.set("ProcSet", Object::Array(procset_array));
        }

        let resources_id = output_doc.add_object(Object::Dictionary(merged_resources));

        // Create the page
        let mut page_dict = Dictionary::new();
        page_dict.set("Type", Object::Name(b"Page".to_vec()));
        page_dict.set("Parent", Object::Reference(output_pages_id));
        page_dict.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(paper_w),
                Object::Real(paper_h),
            ]),
        );
        page_dict.set("Contents", Object::Reference(content_id));
        page_dict.set("Resources", Object::Reference(resources_id));

        let page_id = output_doc.add_object(Object::Dictionary(page_dict));
        output_page_refs.push(Object::Reference(page_id));
    }

    // Create the Pages dictionary
    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Count", Object::Integer(output_page_count as i64));
    pages_dict.set("Kids", Object::Array(output_page_refs.clone()));
    pages_dict.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(paper_w),
            Object::Real(paper_h),
        ]),
    );
    output_doc
        .objects
        .insert(output_pages_id, Object::Dictionary(pages_dict));

    // Create the Catalog
    let catalog_id = output_doc.add_object(Object::Dictionary(Dictionary::from_iter([
        ("Type", Object::Name(b"Catalog".to_vec())),
        ("Pages", Object::Reference(output_pages_id)),
    ])));
    output_doc
        .trailer
        .set("Root", Object::Reference(catalog_id));

    if let Some(tick) = progress.as_mut() {
        tick(Progress::new(
            output_page_count,
            Some(output_page_count),
            "writing output",
        ));
    }

    Ok(Document::from_lopdf(output_doc, None))
}

/// Extract a page as a Form XObject and add it to the target document.
/// Returns (form_id, (width, height), resources_dict).
fn extract_page_as_form(
    source: &mut lopdf::Document,
    target: &mut lopdf::Document,
    page_id: ObjectId,
) -> Result<(ObjectId, (f32, f32), Dictionary)> {
    // Get the page's media box for the Form BBox
    let page_dict = source.get_dictionary(page_id)?;
    let media_box = page_dict.get(b"MediaBox")?;
    let bbox = array_to_rect(media_box)?;
    let (width, height) = (bbox.width(), bbox.height());

    // Get the page's resources
    let resources = page_dict
        .get(b"Resources")
        .ok()
        .and_then(|r| r.as_dict().ok())
        .cloned()
        .unwrap_or_else(Dictionary::new);

    // Get the page's content stream
    let content = source.get_page_content(page_id);

    // Create a Form XObject
    let mut form_dict = Dictionary::new();
    form_dict.set("Type", Object::Name(b"XObject".to_vec()));
    form_dict.set("Subtype", Object::Name(b"Form".to_vec()));
    form_dict.set(
        "BBox",
        Object::Array(vec![
            Object::Real(bbox.x0),
            Object::Real(bbox.y0),
            Object::Real(bbox.x1),
            Object::Real(bbox.y1),
        ]),
    );
    form_dict.set("Resources", Object::Dictionary(resources.clone()));
    form_dict.set(
        "Matrix",
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(1),
            Object::Integer(0),
            Object::Integer(0),
        ]),
    );

    // Copy the content stream
    let form_stream = Stream::new(form_dict, content);
    let form_id = target.add_object(Object::Stream(form_stream));

    Ok((form_id, (width, height), resources))
}

/// Create a booklet from a document.
///
/// Reorders pages into signatures for duplex printing and folding,
/// then applies 2×1 n-up imposition.
pub fn booklet(doc: &Document, options: BookletOptions) -> Result<Document> {
    booklet_with_progress(doc, options, None)
}

/// [`booklet`], reporting progress per output sheet.
pub fn booklet_with_progress(
    doc: &Document,
    options: BookletOptions,
    progress: Option<ProgressFn<'_>>,
) -> Result<Document> {
    options.validate()?;

    let source_pages = doc.page_count();
    if source_pages == 0 {
        return Err(PdfError::EmptySelection);
    }

    // Pad to multiple of signature_size with blank pages
    let padded_pages = source_pages.div_ceil(options.signature_size) * options.signature_size;

    // Generate page order for each signature
    let mut imposed_order = Vec::with_capacity(padded_pages);
    for sig_start in (0..padded_pages).step_by(options.signature_size) {
        let sig_end = (sig_start + options.signature_size).min(padded_pages);
        let sig_pages: Vec<usize> = (sig_start..sig_end).collect();
        let sig_order = compute_signature_order(&sig_pages, options.signature_size);
        imposed_order.extend(sig_order);
    }

    // Now we have the imposed page order. We need to place them 2-up on sheets.
    // For booklet, it's always 2×1 (two pages per sheet).
    // Short-edge binding: landscape sheets, pages side by side (1 row, 2 cols)
    // Long-edge binding: portrait sheets, pages top/bottom (2 rows, 1 col)
    let nup_options = NupOptions {
        rows: if options.binding == Binding::LongEdge {
            2
        } else {
            1
        },
        cols: if options.binding == Binding::LongEdge {
            1
        } else {
            2
        },
        paper_size: options.paper_size,
        margins: options.margins,
        gap: options.gap,
        border: options.border,
        page_order: PageOrder::RowMajor,
        fit: options.fit,
    };

    // Create a temporary document with pages in imposed order. Padding slots
    // past the last real page are mapped to one blank page.
    let temp_doc = create_document_with_page_order(doc, &imposed_order, source_pages)?;

    // Apply n-up
    nup_with_progress(&temp_doc, nup_options, progress)
}

/// Compute the page order for a single signature.
/// For an 8-page signature (short-edge): 8,1,2,7,6,3,4,5
fn compute_signature_order(sig_pages: &[usize], signature_size: usize) -> Vec<usize> {
    let n = signature_size;
    let mut order = Vec::with_capacity(n);

    // Classic booklet imposition algorithm for short-edge binding
    for i in 0..n / 2 {
        if i % 2 == 0 {
            // Even sheet: back side first
            order.push(*sig_pages.get(n - 1 - i).unwrap_or(&usize::MAX));
            order.push(*sig_pages.get(i).unwrap_or(&usize::MAX));
        } else {
            // Odd sheet: front side first
            order.push(*sig_pages.get(i).unwrap_or(&usize::MAX));
            order.push(*sig_pages.get(n - 1 - i).unwrap_or(&usize::MAX));
        }
    }

    // Filter out placeholder pages (usize::MAX)
    order.into_iter().filter(|&p| p != usize::MAX).collect()
}

/// Create a new document with pages in the specified order.
///
/// Any index at or past `source_pages` is a padding slot and becomes a single
/// blank page appended after the real pages.
fn create_document_with_page_order(
    doc: &Document,
    order: &[usize],
    source_pages: usize,
) -> Result<Document> {
    let with_blank = crate::assemble::assemble(vec![
        crate::assemble::Source {
            doc: doc.inner.clone(),
            pages: (0..source_pages).collect(),
        },
        crate::assemble::Source::all(create_blank_page_doc()),
    ])?;

    let pages: Vec<usize> = order.iter().map(|&p| p.min(source_pages)).collect();
    let assembled = crate::assemble::assemble(vec![crate::assemble::Source {
        doc: with_blank,
        pages,
    }])?;

    Ok(Document::from_lopdf(assembled, None))
}

/// Create a simple one-page blank document.
fn create_blank_page_doc() -> lopdf::Document {
    let mut doc = lopdf::Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Count", Object::Integer(1));

    let page_id = doc.new_object_id();
    let mut page_dict = Dictionary::new();
    page_dict.set("Type", Object::Name(b"Page".to_vec()));
    page_dict.set("Parent", Object::Reference(pages_id));
    page_dict.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(595.276),
            Object::Real(841.89),
        ]),
    );
    page_dict.set("Resources", Object::Dictionary(Dictionary::new()));
    page_dict.set(
        "Contents",
        Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
    );

    doc.objects.insert(page_id, Object::Dictionary(page_dict));
    pages_dict.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));

    let catalog_id = doc.add_object(Object::Dictionary(Dictionary::from_iter([
        ("Type", Object::Name(b"Catalog".to_vec())),
        ("Pages", Object::Reference(pages_id)),
    ])));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc
}

/// Convert a PDF array to a Rect.
fn array_to_rect(object: &Object) -> Result<Rect> {
    let Object::Array(values) = object else {
        return Err(PdfError::InvalidMediaBox {
            reason: "the box is not an array of four numbers".into(),
        });
    };
    if values.len() != 4 {
        return Err(PdfError::InvalidMediaBox {
            reason: format!("the box has {} entries, expected 4", values.len()),
        });
    }
    let mut numbers = [0.0f32; 4];
    for (index, value) in values.iter().enumerate() {
        numbers[index] = match value {
            Object::Integer(number) => *number as f32,
            Object::Real(number) => *number,
            _ => {
                return Err(PdfError::InvalidMediaBox {
                    reason: "the box entries must be numbers".into(),
                })
            }
        };
    }
    if !numbers.iter().all(|number| number.is_finite()) {
        return Err(PdfError::InvalidMediaBox {
            reason: "the box entries must be finite numbers".into(),
        });
    }
    Ok(Rect {
        x0: numbers[0],
        y0: numbers[1],
        x1: numbers[2],
        y1: numbers[3],
    })
}

/// A rectangle in PDF points.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn width(&self) -> f32 {
        self.x1 - self.x0
    }
    fn height(&self) -> f32 {
        self.y1 - self.y0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document as FoolscapDocument;
    use lopdf::{dictionary, Document, Object, Stream};
    use std::str::FromStr;

    fn build_test_doc(page_count: usize) -> lopdf::Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();

        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });

        let media_box: Vec<Object> = vec![0.into(), 0.into(), 595.into(), 842.into()];

        let mut kids = Vec::with_capacity(page_count);
        for _ in 1..=page_count {
            let content = Stream::new(dictionary! {}, vec![]);
            let content_id = doc.add_object(content);

            let page = dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => Object::Reference(resources_id),
                "MediaBox" => Object::Array(media_box.clone()),
            };

            kids.push(Object::Reference(doc.add_object(page)));
        }

        let pages = dictionary! {
            "Type" => "Pages",
            "Count" => page_count as i64,
            "Kids" => Object::Array(kids),
        };

        doc.objects.insert(pages_id, Object::Dictionary(pages));

        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);

        doc
    }

    #[test]
    fn nup_options_default() {
        let opts = NupOptions::default();
        assert_eq!(opts.rows, 2);
        assert_eq!(opts.cols, 2);
        assert_eq!(opts.paper_size, PageSize::A4);
        assert_eq!(opts.page_order, PageOrder::RowMajor);
    }

    #[test]
    fn nup_options_validate_rejects_zero() {
        let mut opts = NupOptions {
            rows: 0,
            ..Default::default()
        };
        assert!(opts.validate().is_err());
        opts.rows = 2;
        opts.cols = 0;
        assert!(opts.validate().is_err());
    }

    #[test]
    fn nup_options_validate_rejects_large() {
        let mut opts = NupOptions {
            rows: 11,
            ..Default::default()
        };
        assert!(opts.validate().is_err());
        opts.rows = 2;
        opts.cols = 11;
        assert!(opts.validate().is_err());
    }

    #[test]
    fn booklet_options_default() {
        let opts = BookletOptions::default();
        assert_eq!(opts.signature_size, 8);
        assert_eq!(opts.paper_size, PageSize::A4);
        assert_eq!(opts.binding, Binding::ShortEdge);
    }

    #[test]
    fn booklet_options_validate_rejects_non_multiple_of_4() {
        let mut opts = BookletOptions {
            signature_size: 6,
            ..Default::default()
        };
        assert!(opts.validate().is_err());
        opts.signature_size = 8;
        assert!(opts.validate().is_ok());
    }

    #[test]
    fn page_order_parsing() {
        assert_eq!(PageOrder::from_str("row").unwrap(), PageOrder::RowMajor);
        assert_eq!(
            PageOrder::from_str("ROW-MAJOR").unwrap(),
            PageOrder::RowMajor
        );
        assert_eq!(
            PageOrder::from_str("column").unwrap(),
            PageOrder::ColumnMajor
        );
        assert_eq!(PageOrder::from_str("col").unwrap(), PageOrder::ColumnMajor);
        assert!(PageOrder::from_str("invalid").is_err());
    }

    #[test]
    fn binding_parsing() {
        assert_eq!(Binding::from_str("short").unwrap(), Binding::ShortEdge);
        assert_eq!(Binding::from_str("short-edge").unwrap(), Binding::ShortEdge);
        assert_eq!(Binding::from_str("long").unwrap(), Binding::LongEdge);
        assert_eq!(Binding::from_str("long-edge").unwrap(), Binding::LongEdge);
        assert!(Binding::from_str("invalid").is_err());
    }

    #[test]
    fn signature_order_8_pages() {
        let pages: Vec<usize> = (1..=8).collect();
        let order = compute_signature_order(&pages, 8);
        assert_eq!(order, vec![8, 1, 2, 7, 6, 3, 4, 5]);
    }

    #[test]
    fn signature_order_4_pages() {
        let pages: Vec<usize> = (1..=4).collect();
        let order = compute_signature_order(&pages, 4);
        assert_eq!(order, vec![4, 1, 2, 3]);
    }

    #[test]
    fn signature_order_16_pages() {
        let pages: Vec<usize> = (1..=16).collect();
        let order = compute_signature_order(&pages, 16);
        assert_eq!(
            order,
            vec![16, 1, 2, 15, 14, 3, 4, 13, 12, 5, 6, 11, 10, 7, 8, 9]
        );
    }

    #[test]
    fn nup_2x2_basic() {
        let doc = build_test_doc(4);
        let foolscap_doc = FoolscapDocument::from_lopdf(doc, None);

        let options = NupOptions {
            rows: 2,
            cols: 2,
            paper_size: PageSize::A4,
            margins: Margins::default(),
            gap: 0.0,
            border: false,
            page_order: PageOrder::RowMajor,
            fit: Fit::Contain,
        };

        let result = nup(&foolscap_doc, options).unwrap();
        assert_eq!(result.page_count(), 1);
    }

    #[test]
    fn nup_2x1_basic() {
        let doc = build_test_doc(4);
        let foolscap_doc = FoolscapDocument::from_lopdf(doc, None);

        let options = NupOptions {
            rows: 2,
            cols: 1,
            paper_size: PageSize::A4,
            margins: Margins::default(),
            gap: 0.0,
            border: false,
            page_order: PageOrder::RowMajor,
            fit: Fit::Contain,
        };

        let result = nup(&foolscap_doc, options).unwrap();
        assert_eq!(result.page_count(), 2);
    }

    #[test]
    fn booklet_8_pages() {
        let doc = build_test_doc(8);
        let foolscap_doc = FoolscapDocument::from_lopdf(doc, None);

        let options = BookletOptions {
            signature_size: 8,
            paper_size: PageSize::A4,
            binding: Binding::ShortEdge,
            margins: Margins::default(),
            gap: 0.0,
            border: false,
            fit: Fit::Contain,
        };

        let result = booklet(&foolscap_doc, options).unwrap();
        // 8 pages -> 4 sheets (2 pages per sheet)
        assert_eq!(result.page_count(), 4);
    }

    #[test]
    fn booklet_16_pages() {
        let doc = build_test_doc(16);
        let foolscap_doc = FoolscapDocument::from_lopdf(doc, None);

        let options = BookletOptions {
            signature_size: 8,
            paper_size: PageSize::A4,
            binding: Binding::ShortEdge,
            margins: Margins::default(),
            gap: 0.0,
            border: false,
            fit: Fit::Contain,
        };

        let result = booklet(&foolscap_doc, options).unwrap();
        // 16 pages -> 8 sheets (2 pages per sheet)
        assert_eq!(result.page_count(), 8);
    }

    #[test]
    fn booklet_pads_a_partial_signature_with_blank_pages() {
        // 5 real pages fill one 8-slot signature; the last three slots are blank.
        let doc = build_test_doc(5);
        let foolscap_doc = FoolscapDocument::from_lopdf(doc, None);

        let options = BookletOptions {
            signature_size: 8,
            paper_size: PageSize::A4,
            binding: Binding::ShortEdge,
            margins: Margins::default(),
            gap: 0.0,
            border: false,
            fit: Fit::Contain,
        };

        let result = booklet(&foolscap_doc, options).unwrap();
        // 8 slots -> 4 sheets (2 pages per sheet)
        assert_eq!(result.page_count(), 4);
    }
}
