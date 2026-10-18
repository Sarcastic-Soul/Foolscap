//! Text and image stamping: watermarks, page numbers, headers and footers.
//!
//! One engine behind four CLI faces. Every stamp is a small content stream
//! drawn over or under the selected pages, built with `lopdf` alone: a `q`
//! block that sets an opacity graphics state, selects a Base14 font (never
//! embedded, so Latin v1 text works everywhere), positions with `Tm` and draws
//! with `Tj` — or draws an embedded image XObject with `Do`.

use std::str::FromStr;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream};

use super::contents::{append_content, prepend_content};
use crate::assemble::materialize_inherited_attributes;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

/// Resource names this module adds (`FsWm0`, `GsA`, `FmWm0`) carry a prefix
/// nobody else uses. Imported names share a namespace with the page's own
/// `/F1` — the most popular font name in existence — so an unprefixed name
/// would silently repoint the page's real text at the stamp font.
const FONT_NAME: &str = "FsWm0";
const GS_NAME: &str = "GsA";
/// Name of the stamp image XObject in the page resources.
const IMAGE_NAME: &str = "FmWm0";

/// A point in PDF coordinates (points, origin at bottom-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A single stroke: an ordered list of points.
pub type Stroke = Vec<Point>;

/// What gets drawn on each selected page.
#[derive(Debug, Clone)]
pub enum StampContent {
    /// Latin text. `{n}` expands to the one-indexed page number and `{total}`
    /// to the document page count, per page.
    Text(String),
    /// Encoded image bytes (PNG or JPEG), decoded with the `image` crate.
    Image(Vec<u8>),
    /// Freehand signature strokes. Each stroke is drawn as a path: `m l l ... S`.
    Signature(Vec<Stroke>),
}

/// One of nine positions on the page, before `offset` is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    #[default]
    Center,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl FromStr for Anchor {
    type Err = PdfError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "top-left" => Ok(Anchor::TopLeft),
            "top-center" | "top-centre" => Ok(Anchor::TopCenter),
            "top-right" => Ok(Anchor::TopRight),
            "middle-left" | "left" => Ok(Anchor::MiddleLeft),
            "center" | "centre" | "middle" => Ok(Anchor::Center),
            "middle-right" | "right" => Ok(Anchor::MiddleRight),
            "bottom-left" => Ok(Anchor::BottomLeft),
            "bottom-center" | "bottom-centre" => Ok(Anchor::BottomCenter),
            "bottom-right" => Ok(Anchor::BottomRight),
            other => Err(PdfError::InvalidStamp(format!(
                "unknown anchor {other:?}; expected one of top-left, top-center, top-right, \
                 middle-left, center, middle-right, bottom-left, bottom-center, bottom-right"
            ))),
        }
    }
}

/// Whether the stamp draws above the page content or beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StampLayer {
    /// Draw after the page content, on top of it.
    #[default]
    Over,
    /// Draw before the page content, underneath it.
    Under,
}

impl FromStr for StampLayer {
    type Err = PdfError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "over" => Ok(StampLayer::Over),
            "under" => Ok(StampLayer::Under),
            other => Err(PdfError::InvalidStamp(format!(
                "unknown layer {other:?}; expected \"over\" or \"under\""
            ))),
        }
    }
}

/// Which Base14 font the stamp text uses. Neither is embedded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StampFont {
    #[default]
    Helvetica,
    Courier,
}

impl FromStr for StampFont {
    type Err = PdfError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "helvetica" => Ok(StampFont::Helvetica),
            "courier" => Ok(StampFont::Courier),
            other => Err(PdfError::InvalidStamp(format!(
                "unknown font {other:?}; expected \"helvetica\" or \"courier\""
            ))),
        }
    }
}

impl StampFont {
    fn base_name(&self) -> &'static str {
        match self {
            StampFont::Helvetica => "Helvetica",
            StampFont::Courier => "Courier",
        }
    }
}

/// How and where a stamp is drawn.
#[derive(Debug, Clone)]
pub struct StampOptions {
    /// The text, image, or signature to draw.
    pub content: StampContent,
    /// Nine-way position on the page.
    pub anchor: Anchor,
    /// Extra shift in points, added after the anchor is resolved.
    pub offset: (f32, f32),
    /// Inset from the page edges for edge anchors, in points.
    pub margin: f32,
    /// Counter-clockwise rotation in degrees, about the anchor point.
    pub rotation_deg: f32,
    /// Opacity in `0.0..=1.0`. Fully opaque draws without a graphics state.
    pub opacity: f32,
    /// Text size in points. Ignored for images and signatures.
    pub font_size: f32,
    /// Text colour as red, green, blue in `0.0..=1.0`. Ignored for images.
    pub color: (f32, f32, f32),
    /// Which Base14 font text uses.
    pub font: StampFont,
    /// Above or beneath the page content.
    pub layer: StampLayer,
    /// Image draw width as a multiple of its pixel size at 72 dpi.
    /// Ignored for text and signatures.
    pub image_scale: f32,
    /// Signature stroke colour as red, green, blue in `0.0..=1.0`.
    pub stroke_color: (f32, f32, f32),
    /// Signature stroke width in points.
    pub stroke_width: f32,
}

impl Default for StampOptions {
    fn default() -> Self {
        Self {
            content: StampContent::Text(String::new()),
            anchor: Anchor::Center,
            offset: (0.0, 0.0),
            margin: 36.0,
            rotation_deg: 0.0,
            opacity: 1.0,
            font_size: 12.0,
            color: (0.0, 0.0, 0.0),
            font: StampFont::Helvetica,
            layer: StampLayer::Over,
            image_scale: 1.0,
            stroke_color: (0.0, 0.0, 0.0),
            stroke_width: 2.0,
        }
    }
}

impl StampOptions {
    /// A centred text stamp.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: StampContent::Text(text.into()),
            ..Self::default()
        }
    }

    /// A centred image stamp from encoded bytes.
    pub fn image(bytes: Vec<u8>) -> Self {
        Self {
            content: StampContent::Image(bytes),
            ..Self::default()
        }
    }

    /// A centred signature stamp from strokes.
    pub fn signature(strokes: Vec<Stroke>) -> Self {
        Self {
            content: StampContent::Signature(strokes),
            ..Self::default()
        }
    }
}

/// Draw `opts` onto `pages`.
///
/// Selecting the same page twice stamps it once.
pub fn stamp(doc: &mut Document, pages: &PageRange, opts: &StampOptions) -> Result<()> {
    stamp_with_progress(doc, pages, opts, None)
}

/// [`stamp`], reporting one progress tick per stamped page.
pub fn stamp_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    opts: &StampOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    validate(opts)?;

    let page_count = doc.page_count();
    let selected = pages.resolve_unique(page_count)?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    // Pages can inherit /Resources and /MediaBox from the page tree. Reading
    // the inherited values and writing the result onto the page itself keeps
    // unselected siblings unchanged, as in rotate.
    materialize_inherited_attributes(&mut doc.inner)?;

    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();

    let font_id = add_stamp_font(&mut doc.inner, opts.font)?;
    let gs_id = if is_transparent(opts.opacity) {
        Some(add_opacity_state(&mut doc.inner, opts.opacity)?)
    } else {
        None
    };
    let image_id = match &opts.content {
        StampContent::Image(bytes) => Some(embed_image(&mut doc.inner, bytes)?),
        _ => None,
    };

    let total = page_count;
    for (done, position) in selected.iter().enumerate() {
        if let Some(report) = progress.as_mut() {
            report(Progress::new(
                done,
                Some(selected.len()),
                format!("stamping page {}", position + 1),
            ));
        }

        let page_id = page_ids[*position];
        let (page_w, page_h) = page_size(&doc.inner, page_id);
        let number = position + 1;

        let stream = match &opts.content {
            StampContent::Text(template) => {
                let text = expand_template(template, number, total);
                text_stream(&text, opts, page_w, page_h)
            }
            StampContent::Image(bytes) => {
                let (pixels_w, pixels_h) = image_dimensions(bytes)?;
                image_stream(opts, page_w, page_h, pixels_w, pixels_h)
            }
            StampContent::Signature(strokes) => signature_stream(strokes, opts, page_w, page_h),
        }?;

        let layer_id = doc.inner.add_object(Stream::new(Dictionary::new(), stream));
        match opts.layer {
            StampLayer::Over => append_content(&mut doc.inner, page_id, layer_id)?,
            StampLayer::Under => prepend_content(&mut doc.inner, page_id, layer_id)?,
        }
        merge_resources(
            &mut doc.inner,
            page_id,
            Some(font_id),
            gs_id,
            image_id,
            matches!(opts.content, StampContent::Image(_)),
        )?;
    }

    if let Some(report) = progress.as_mut() {
        report(Progress::new(
            selected.len(),
            Some(selected.len()),
            "stamping done",
        ));
    }

    Ok(())
}

fn validate(opts: &StampOptions) -> Result<()> {
    if !(0.0..=1.0).contains(&opts.opacity) {
        return Err(PdfError::InvalidOpacity(opts.opacity));
    }
    if !opts.font_size.is_finite() || opts.font_size <= 0.0 {
        return Err(PdfError::InvalidStamp(format!(
            "font size must be positive, got {}",
            opts.font_size
        )));
    }
    if opts.margin < 0.0 || !opts.margin.is_finite() {
        return Err(PdfError::InvalidStamp(format!(
            "margin must be zero or positive, got {}",
            opts.margin
        )));
    }
    if !opts.image_scale.is_finite() || opts.image_scale <= 0.0 {
        return Err(PdfError::InvalidStamp(format!(
            "image scale must be positive, got {}",
            opts.image_scale
        )));
    }
    if !opts.stroke_width.is_finite() || opts.stroke_width <= 0.0 {
        return Err(PdfError::InvalidStamp(format!(
            "stroke width must be positive, got {}",
            opts.stroke_width
        )));
    }
    for (name, value) in [
        ("red", opts.color.0),
        ("green", opts.color.1),
        ("blue", opts.color.2),
    ] {
        if !(0.0..=1.0).contains(&value) {
            return Err(PdfError::InvalidStamp(format!(
                "{name} channel {value} is outside 0.0..=1.0"
            )));
        }
    }
    for (name, value) in [
        ("stroke red", opts.stroke_color.0),
        ("stroke green", opts.stroke_color.1),
        ("stroke blue", opts.stroke_color.2),
    ] {
        if !(0.0..=1.0).contains(&value) {
            return Err(PdfError::InvalidStamp(format!(
                "{name} channel {value} is outside 0.0..=1.0"
            )));
        }
    }
    match &opts.content {
        StampContent::Text(text) if text.is_empty() => {
            Err(PdfError::InvalidStamp("stamp text is empty".into()))
        }
        StampContent::Image(bytes) if bytes.is_empty() => {
            Err(PdfError::InvalidStamp("stamp image is empty".into()))
        }
        StampContent::Signature(strokes) if strokes.is_empty() => Err(PdfError::InvalidStamp(
            "stamp signature has no strokes".into(),
        )),
        StampContent::Signature(strokes) if strokes.iter().all(|s| s.is_empty()) => Err(
            PdfError::InvalidStamp("stamp signature strokes are empty".into()),
        ),
        _ => Ok(()),
    }
}

fn is_transparent(opacity: f32) -> bool {
    opacity < 1.0 - 1e-6
}

/// Expand `{n}` to the one-indexed page number and `{total}` to the page
/// count. Anything else is left alone.
pub fn expand_template(template: &str, number: usize, total: usize) -> String {
    template
        .replace("{total}", &total.to_string())
        .replace("{n}", &number.to_string())
}

/// Rough width of Latin text in a Base14 font, in points.
///
/// Proportional fonts vary per glyph; half an em per character centres short
/// stamps well enough for a v1 that never embeds a font.
pub fn estimate_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().count() as f32 * font_size * 0.5
}

/// Cosine and sine of `degrees` as `[cos, sin, -sin, cos]`, the four
/// coefficients of a counter-clockwise rotation matrix.
pub fn rotation_matrix(degrees: f32) -> [f32; 4] {
    let radians = degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    [cos, sin, -sin, cos]
}

/// Where the lower-left corner of a `content_w` by `content_h` box sits for
/// `anchor` on a `page_w` by `page_h` page, before `offset` is added.
pub fn anchor_point(
    anchor: Anchor,
    page_w: f32,
    page_h: f32,
    content_w: f32,
    content_h: f32,
    margin: f32,
) -> (f32, f32) {
    let x = match anchor {
        Anchor::TopLeft | Anchor::MiddleLeft | Anchor::BottomLeft => margin,
        Anchor::TopCenter | Anchor::Center | Anchor::BottomCenter => (page_w - content_w) / 2.0,
        Anchor::TopRight | Anchor::MiddleRight | Anchor::BottomRight => page_w - margin - content_w,
    };
    let y = match anchor {
        Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight => margin,
        Anchor::MiddleLeft | Anchor::Center | Anchor::MiddleRight => (page_h - content_h) / 2.0,
        Anchor::TopLeft | Anchor::TopCenter | Anchor::TopRight => page_h - margin - content_h,
    };
    (x, y)
}

fn text_stream(text: &str, opts: &StampOptions, page_w: f32, page_h: f32) -> Result<Vec<u8>> {
    let width = estimate_text_width(text, opts.font_size);
    let (mut x, mut y) = anchor_point(
        opts.anchor,
        page_w,
        page_h,
        width,
        opts.font_size,
        opts.margin,
    );
    x += opts.offset.0;
    y += opts.offset.1;

    let [a, b, c, d] = rotation_matrix(opts.rotation_deg);
    let (r, g, bl) = opts.color;

    let mut operations = vec![Operation::new("q", vec![])];
    if is_transparent(opts.opacity) {
        operations.push(Operation::new(
            "gs",
            vec![Object::Name(GS_NAME.as_bytes().to_vec())],
        ));
    }
    operations.push(Operation::new("BT", vec![]));
    operations.push(Operation::new(
        "Tf",
        vec![
            Object::Name(FONT_NAME.as_bytes().to_vec()),
            Object::Real(opts.font_size),
        ],
    ));
    operations.push(Operation::new(
        "rg",
        vec![Object::Real(r), Object::Real(g), Object::Real(bl)],
    ));
    operations.push(Operation::new(
        "Tm",
        vec![
            Object::Real(a),
            Object::Real(b),
            Object::Real(c),
            Object::Real(d),
            Object::Real(x),
            Object::Real(y),
        ],
    ));
    operations.push(Operation::new(
        "Tj",
        vec![Object::string_literal(text.as_bytes().to_vec())],
    ));
    operations.push(Operation::new("ET", vec![]));
    operations.push(Operation::new("Q", vec![]));

    Content { operations }.encode().map_err(|source| {
        PdfError::Internal(format!("could not encode the stamp content: {source}"))
    })
}

fn image_stream(
    opts: &StampOptions,
    page_w: f32,
    page_h: f32,
    pixels_w: u32,
    pixels_h: u32,
) -> Result<Vec<u8>> {
    let draw_w = pixels_w as f32 * opts.image_scale;
    let draw_h = pixels_h as f32 * opts.image_scale;
    let (mut x, mut y) = anchor_point(opts.anchor, page_w, page_h, draw_w, draw_h, opts.margin);
    x += opts.offset.0;
    y += opts.offset.1;

    // cm maps the unit square to the drawn image: scale, rotate about the
    // anchor point, then translate there.
    let [cos, sin, neg_sin, cos2] = rotation_matrix(opts.rotation_deg);
    let mut operations = vec![Operation::new("q", vec![])];
    if is_transparent(opts.opacity) {
        operations.push(Operation::new(
            "gs",
            vec![Object::Name(GS_NAME.as_bytes().to_vec())],
        ));
    }
    operations.push(Operation::new(
        "cm",
        vec![
            Object::Real(draw_w * cos),
            Object::Real(draw_w * sin),
            Object::Real(draw_h * neg_sin),
            Object::Real(draw_h * cos2),
            Object::Real(x),
            Object::Real(y),
        ],
    ));
    operations.push(Operation::new(
        "Do",
        vec![Object::Name(IMAGE_NAME.as_bytes().to_vec())],
    ));
    operations.push(Operation::new("Q", vec![]));

    Content { operations }.encode().map_err(|source| {
        PdfError::Internal(format!("could not encode the stamp content: {source}"))
    })
}

/// Generate a content stream for signature strokes.
/// Each stroke is drawn as a path: `m l l ... S` (move, line-to, stroke).
fn signature_stream(
    strokes: &[Stroke],
    opts: &StampOptions,
    page_w: f32,
    page_h: f32,
) -> Result<Vec<u8>> {
    // Compute the bounding box of all strokes to position them via anchor.
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for stroke in strokes {
        for point in stroke {
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
        }
    }

    // Handle edge case of empty strokes (should be caught by validate, but be safe)
    if min_x.is_infinite() || min_y.is_infinite() {
        return Ok(vec![]);
    }

    let content_w = max_x - min_x;
    let content_h = max_y - min_y;

    // Position the signature's bounding box according to anchor.
    let (mut x, mut y) = anchor_point(
        opts.anchor,
        page_w,
        page_h,
        content_w,
        content_h,
        opts.margin,
    );
    x += opts.offset.0;
    y += opts.offset.1;

    // Apply rotation about the anchor point.
    let [a, b, c, d] = rotation_matrix(opts.rotation_deg);
    let (r, g, bl) = opts.stroke_color;

    let mut operations = vec![Operation::new("q", vec![])];
    if is_transparent(opts.opacity) {
        operations.push(Operation::new(
            "gs",
            vec![Object::Name(GS_NAME.as_bytes().to_vec())],
        ));
    }
    // Set stroke color and width
    operations.push(Operation::new(
        "RG",
        vec![Object::Real(r), Object::Real(g), Object::Real(bl)],
    ));
    operations.push(Operation::new("w", vec![Object::Real(opts.stroke_width)]));
    // Set line cap to round for smoother signatures
    operations.push(Operation::new("J", vec![Object::Integer(1)])); // 1 = round cap
                                                                    // Set line join to round
    operations.push(Operation::new("j", vec![Object::Integer(1)])); // 1 = round join

    // Apply transform: translate to anchor, rotate, then translate by -min to align bbox
    operations.push(Operation::new(
        "cm",
        vec![
            Object::Real(a),
            Object::Real(b),
            Object::Real(c),
            Object::Real(d),
            Object::Real(x - a * min_x - c * min_y),
            Object::Real(y - b * min_x - d * min_y),
        ],
    ));

    // Draw each stroke
    for stroke in strokes {
        if stroke.is_empty() {
            continue;
        }
        // moveto first point
        operations.push(Operation::new(
            "m",
            vec![Object::Real(stroke[0].x), Object::Real(stroke[0].y)],
        ));
        // lineto remaining points
        for point in &stroke[1..] {
            operations.push(Operation::new(
                "l",
                vec![Object::Real(point.x), Object::Real(point.y)],
            ));
        }
        // stroke the path
        operations.push(Operation::new("S", vec![]));
    }

    operations.push(Operation::new("Q", vec![]));

    Content { operations }.encode().map_err(|source| {
        PdfError::Internal(format!("could not encode the signature content: {source}"))
    })
}

fn add_stamp_font(doc: &mut lopdf::Document, font: StampFont) -> Result<ObjectId> {
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"Font".to_vec()));
    dict.set("Subtype", Object::Name(b"Type1".to_vec()));
    dict.set(
        "BaseFont",
        Object::Name(font.base_name().as_bytes().to_vec()),
    );
    dict.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    Ok(doc.add_object(Object::Dictionary(dict)))
}

fn add_opacity_state(doc: &mut lopdf::Document, opacity: f32) -> Result<ObjectId> {
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"ExtGState".to_vec()));
    dict.set("ca", Object::Real(opacity));
    dict.set("CA", Object::Real(opacity));
    Ok(doc.add_object(Object::Dictionary(dict)))
}

/// Decode the image once and embed it as a Flate-compressed RGB XObject.
fn embed_image(doc: &mut lopdf::Document, bytes: &[u8]) -> Result<ObjectId> {
    use std::io::Write;

    let decoded = image::load_from_memory(bytes)
        .map_err(|_| PdfError::UnsupportedImage(std::path::PathBuf::from("stamp image")))?;
    let rgb = decoded.to_rgb8();
    let (width, height) = (rgb.width(), rgb.height());

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(rgb.as_raw())
        .map_err(|source| PdfError::Internal(format!("could not compress the stamp: {source}")))?;
    let deflated = encoder
        .finish()
        .map_err(|source| PdfError::Internal(format!("could not compress the stamp: {source}")))?;

    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", Object::Integer(width as i64));
    dict.set("Height", Object::Integer(height as i64));
    dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));

    let mut stream = Stream::new(dict, deflated);
    // Already deflated; compressing again on save would corrupt it.
    stream.allows_compression = false;
    Ok(doc.add_object(stream))
}

fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    let decoded = image::load_from_memory(bytes)
        .map_err(|_| PdfError::UnsupportedImage(std::path::PathBuf::from("stamp image")))?;
    Ok((decoded.width(), decoded.height()))
}

/// Effective page size in points, following `/MediaBox` up the page tree.
fn page_size(doc: &lopdf::Document, page_id: ObjectId) -> (f32, f32) {
    let mut current = Some(page_id);
    for _ in 0..64 {
        let Some(id) = current else { break };
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
        if let Ok(Object::Array(values)) = dict.get(b"MediaBox") {
            if values.len() == 4 {
                let number = |object: &Object| match object {
                    Object::Integer(v) => Some(*v as f32),
                    Object::Real(v) => Some(*v),
                    _ => None,
                };
                if let (Some(x0), Some(y0), Some(x1), Some(y1)) = (
                    values.first().and_then(number),
                    values.get(1).and_then(number),
                    values.get(2).and_then(number),
                    values.get(3).and_then(number),
                ) {
                    return (x1 - x0, y1 - y0);
                }
            }
        }
        current = dict
            .get(b"Parent")
            .ok()
            .and_then(|object| object.as_reference().ok());
    }
    (595.0, 842.0)
}

fn page_resources(doc: &lopdf::Document, page_id: ObjectId) -> Dictionary {
    let mut current = Some(page_id);
    for _ in 0..64 {
        let Some(id) = current else { break };
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
        if let Ok(resources) = dict.get(b"Resources") {
            match resources {
                Object::Dictionary(dict) => return dict.clone(),
                Object::Reference(id) => {
                    if let Ok(dict) = doc.get_dictionary(*id) {
                        return dict.clone();
                    }
                }
                _ => {}
            }
        }
        current = dict
            .get(b"Parent")
            .ok()
            .and_then(|object| object.as_reference().ok());
    }
    Dictionary::new()
}

fn resolve_sub_dictionary(doc: &lopdf::Document, dict: &Dictionary, key: &[u8]) -> Dictionary {
    match dict.get(key) {
        Ok(Object::Dictionary(inner)) => inner.clone(),
        Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok().cloned().unwrap_or_default(),
        _ => Dictionary::new(),
    }
}

/// Point the page at the stamp font, opacity state and image, keeping its own
/// entries. Resources are inheritable, so a fresh dictionary goes on the page
/// itself rather than editing one shared with every other page.
fn merge_resources(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    font_id: Option<ObjectId>,
    gs_id: Option<ObjectId>,
    image_id: Option<ObjectId>,
    is_image: bool,
) -> Result<()> {
    let inherited = page_resources(doc, page_id);
    let mut resources = inherited;

    if let Some(font_id) = font_id {
        if !is_image {
            let mut fonts = resolve_sub_dictionary(doc, &resources, b"Font");
            fonts.set(FONT_NAME.as_bytes().to_vec(), Object::Reference(font_id));
            resources.set("Font", Object::Dictionary(fonts));
        }
    }

    if let Some(gs_id) = gs_id {
        let mut states = resolve_sub_dictionary(doc, &resources, b"ExtGState");
        states.set(GS_NAME.as_bytes().to_vec(), Object::Reference(gs_id));
        resources.set("ExtGState", Object::Dictionary(states));
    }

    if let Some(image_id) = image_id {
        if is_image {
            let mut xobjects = resolve_sub_dictionary(doc, &resources, b"XObject");
            xobjects.set(IMAGE_NAME.as_bytes().to_vec(), Object::Reference(image_id));
            resources.set("XObject", Object::Dictionary(xobjects));
        }
    }

    let resources_id = doc.add_object(Object::Dictionary(resources));
    let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
    page.set("Resources", Object::Reference(resources_id));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_resolve_to_corners_and_centres() {
        let (w, h) = (600.0, 800.0);
        let (cw, ch) = (100.0, 20.0);
        let margin = 36.0;

        assert_eq!(
            anchor_point(Anchor::BottomLeft, w, h, cw, ch, margin),
            (36.0, 36.0)
        );
        assert_eq!(
            anchor_point(Anchor::TopRight, w, h, cw, ch, margin),
            (600.0 - 36.0 - 100.0, 800.0 - 36.0 - 20.0)
        );
        assert_eq!(
            anchor_point(Anchor::Center, w, h, cw, ch, margin),
            ((600.0 - 100.0) / 2.0, (800.0 - 20.0) / 2.0)
        );
        assert_eq!(
            anchor_point(Anchor::TopCenter, w, h, cw, ch, margin),
            ((600.0 - 100.0) / 2.0, 800.0 - 36.0 - 20.0)
        );
        assert_eq!(
            anchor_point(Anchor::BottomCenter, w, h, cw, ch, margin),
            ((600.0 - 100.0) / 2.0, 36.0)
        );
    }

    #[test]
    fn rotation_matrix_matches_right_angles() {
        let [a, b, c, d] = rotation_matrix(0.0);
        assert!(
            (a - 1.0).abs() < 1e-6 && b.abs() < 1e-6 && c.abs() < 1e-6 && (d - 1.0).abs() < 1e-6
        );

        let [a, b, c, d] = rotation_matrix(90.0);
        assert!(
            a.abs() < 1e-6 && (b - 1.0).abs() < 1e-6 && (c + 1.0).abs() < 1e-6 && d.abs() < 1e-6
        );

        let [a, b, c, d] = rotation_matrix(180.0);
        assert!(
            (a + 1.0).abs() < 1e-6 && b.abs() < 1e-6 && c.abs() < 1e-6 && (d + 1.0).abs() < 1e-6
        );

        // 45 degrees: cos == sin == sqrt(2)/2.
        let [a, b, _, _] = rotation_matrix(45.0);
        let expected = std::f32::consts::FRAC_1_SQRT_2;
        assert!((a - expected).abs() < 1e-6 && (b - expected).abs() < 1e-6);
    }

    #[test]
    fn templates_expand_per_page() {
        assert_eq!(expand_template("Page {n} of {total}", 2, 5), "Page 2 of 5");
        assert_eq!(expand_template("{n}/{total}", 1, 1), "1/1");
        assert_eq!(expand_template("DRAFT", 3, 4), "DRAFT");
    }

    #[test]
    fn bad_options_are_rejected() {
        let mut opts = StampOptions::text("hi");
        opts.opacity = 1.5;
        assert!(matches!(validate(&opts), Err(PdfError::InvalidOpacity(_))));

        let mut opts = StampOptions::text("hi");
        opts.opacity = -0.1;
        assert!(validate(&opts).is_err());

        let opts = StampOptions::text(String::new());
        assert!(validate(&opts).is_err());

        let mut opts = StampOptions::text("hi");
        opts.font_size = 0.0;
        assert!(validate(&opts).is_err());
    }

    #[test]
    fn anchor_names_parse() {
        assert_eq!(Anchor::from_str("center").unwrap(), Anchor::Center);
        assert_eq!(
            Anchor::from_str("BOTTOM-RIGHT").unwrap(),
            Anchor::BottomRight
        );
        assert_eq!(Anchor::from_str("top_left").unwrap(), Anchor::TopLeft);
        assert!(Anchor::from_str("nowhere").is_err());
    }
}
