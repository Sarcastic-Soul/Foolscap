//! Page geometry: cropping, media-box control, and scaling to stock sizes.
//!
//! Cropping here means setting `/CropBox`, the rectangle a viewer displays,
//! clamped inside `/MediaBox`. Content outside the crop is hidden rather than
//! deleted. [`reset_crop`] removes the `/CropBox` again (`--reset`).
//!
//! [`scale_to`] resizes the page itself to a stock size and scales the drawn
//! content to fit it, honouring `/Rotate`: a page with `/Rotate 90` or `270`
//! displays its media box on its side, so the stored media box is the target
//! transposed to keep the displayed size correct.

use lopdf::content::{Content, Operation};
use lopdf::{Object, ObjectId};

use crate::assemble::materialize_inherited_attributes;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

/// Standard page sizes, in PDF points.
///
/// Shared with images-to-PDF conversion so that both paths mean the same thing
/// by `a4`: this module owns the definition, `convert` re-exports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageSize {
    A4,
    Letter,
    Legal,
    /// One page per image, exactly the size of that image at `dpi`. Only
    /// meaningful to images-to-PDF conversion; [`scale_to`] rejects it.
    FitImage,
    /// An explicit size in points.
    Custom {
        width: f32,
        height: f32,
    },
}

impl PageSize {
    /// Size in points, or `None` for [`PageSize::FitImage`], which depends on
    /// the image.
    pub fn points(&self) -> Option<(f32, f32)> {
        match *self {
            PageSize::A4 => Some((595.276, 841.89)),
            PageSize::Letter => Some((612.0, 792.0)),
            PageSize::Legal => Some((612.0, 1008.0)),
            PageSize::FitImage => None,
            PageSize::Custom { width, height } => Some((width, height)),
        }
    }

    /// Parse `a4`, `letter`, `legal`, or a custom `WxH` size in points such as
    /// `595x842`.
    pub fn parse(spec: &str) -> Result<Self> {
        let lowered = spec.trim().to_ascii_lowercase();
        match lowered.as_str() {
            "a4" => Ok(PageSize::A4),
            "letter" => Ok(PageSize::Letter),
            "legal" => Ok(PageSize::Legal),
            _ => {
                let (width, height) = spec.trim().split_once(['x', 'X']).ok_or_else(|| {
                    PdfError::InvalidMediaBox {
                        reason: format!("{spec:?} is not a4, letter, legal, or WxH"),
                    }
                })?;
                let parse = |text: &str| {
                    text.trim()
                        .parse::<f32>()
                        .map_err(|_| PdfError::InvalidMediaBox {
                            reason: format!("{spec:?} is not a valid WxH size"),
                        })
                };
                let (width, height) = (parse(width)?, parse(height)?);
                if width <= 0.0 || height <= 0.0 || !width.is_finite() || !height.is_finite() {
                    return Err(PdfError::InvalidMediaBox {
                        reason: format!("{spec:?} must be a positive WxH size"),
                    });
                }
                Ok(PageSize::Custom { width, height })
            }
        }
    }
}

/// How content is placed on a page larger or smaller than itself.
///
/// Shared with images-to-PDF conversion; see [`PageSize`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fit {
    /// Scale to fit entirely within the page, preserving aspect ratio. Leaves
    /// margins on one axis.
    #[default]
    Contain,
    /// Scale to cover the page, preserving aspect ratio. Crops the overflow.
    Cover,
    /// Scale each axis independently to fill the page. Distorts the image.
    Stretch,
}

impl Fit {
    /// Parse `contain`, `cover`, or `stretch`.
    pub fn parse(spec: &str) -> Result<Self> {
        match spec.trim().to_ascii_lowercase().as_str() {
            "contain" => Ok(Fit::Contain),
            "cover" => Ok(Fit::Cover),
            "stretch" => Ok(Fit::Stretch),
            _ => Err(PdfError::InvalidMediaBox {
                reason: format!("{spec:?} is not contain, cover, or stretch"),
            }),
        }
    }
}

/// A rectangle in PDF points, in unrotated user space: `(x0, y0)` is the lower
/// left and `(x1, y1)` the upper right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    /// Build from an origin plus a size, as typed on a command line.
    pub fn from_xywh(x: f32, y: f32, width: f32, height: f32) -> Result<Self> {
        if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
            return Err(PdfError::InvalidCropBox {
                reason: "coordinates must be finite numbers".into(),
            });
        }
        if width <= 0.0 || height <= 0.0 {
            return Err(PdfError::InvalidCropBox {
                reason: format!("width and height must be positive, got {width}x{height}"),
            });
        }
        Ok(Self {
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + height,
        })
    }

    /// Parse `x,y,w,h`, with commas or whitespace between the four numbers.
    pub fn parse_xywh(spec: &str) -> Result<Self> {
        let numbers: Vec<f32> = spec
            .split([',', ' ', '\t'])
            .filter(|piece| !piece.trim().is_empty())
            .map(|piece| {
                piece
                    .trim()
                    .parse::<f32>()
                    .map_err(|_| PdfError::InvalidCropBox {
                        reason: format!("{spec:?} is not four numbers as x,y,w,h"),
                    })
            })
            .collect::<Result<Vec<_>>>()?;

        if numbers.len() != 4 {
            return Err(PdfError::InvalidCropBox {
                reason: format!("{spec:?} is not four numbers as x,y,w,h"),
            });
        }
        Self::from_xywh(numbers[0], numbers[1], numbers[2], numbers[3])
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// The overlap with `other`, or `None` when they do not touch.
    fn intersect(&self, other: &Rect) -> Option<Rect> {
        let rect = Rect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (rect.x1 > rect.x0 && rect.y1 > rect.y0).then_some(rect)
    }
}

/// Show only `rect` of the selected pages, by setting `/CropBox`.
///
/// The rectangle is clamped to the page's `/MediaBox`: asking for more than
/// the page holds crops to the page edge rather than producing a box no viewer
/// accepts. Both integer and real box entries are read; a zero-area
/// intersection is [`PdfError::InvalidCropBox`].
pub fn crop(doc: &mut Document, pages: &PageRange, rect: Rect) -> Result<()> {
    crop_with_progress(doc, pages, rect, None)
}

/// [`crop`], reporting one progress tick per selected page.
pub fn crop_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    rect: Rect,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(PdfError::InvalidCropBox {
            reason: "the crop box must have a positive width and height".into(),
        });
    }

    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    // Pages may inherit /MediaBox and /CropBox from the page tree; materialise
    // them first so every page is clamped against its own effective box and a
    // shared parent is never edited in place.
    materialize_inherited_attributes(&mut doc.inner)?;
    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();
    let total = selected.len();

    for (index, position) in selected.iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(total),
                format!("cropping page {}", position + 1),
            ));
        }
        let page_id = page_ids[*position];
        let media = page_box(&doc.inner, page_id, b"MediaBox")?.ok_or_else(|| {
            PdfError::InvalidMediaBox {
                reason: format!("page {} has no /MediaBox", position + 1),
            }
        })?;

        let clamped = rect
            .intersect(&media)
            .ok_or_else(|| PdfError::InvalidCropBox {
                reason: format!(
                    "the crop box [{}, {}, {}, {}] does not overlap page {} [{}, {}, {}, {}]",
                    rect.x0,
                    rect.y0,
                    rect.x1,
                    rect.y1,
                    position + 1,
                    media.x0,
                    media.y0,
                    media.x1,
                    media.y1,
                ),
            })?;

        let page = doc.inner.get_object_mut(page_id)?.as_dict_mut()?;
        page.set("CropBox", box_array(clamped));
    }

    Ok(())
}

/// Set `/MediaBox` on the selected pages, clamping any `/CropBox` back inside.
///
/// Both integer and real entries are accepted for the existing boxes.
pub fn set_mediabox(doc: &mut Document, pages: &PageRange, rect: Rect) -> Result<()> {
    set_mediabox_with_progress(doc, pages, rect, None)
}

/// [`set_mediabox`], reporting one progress tick per selected page.
pub fn set_mediabox_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    rect: Rect,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(PdfError::InvalidMediaBox {
            reason: "the media box must have a positive width and height".into(),
        });
    }

    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    materialize_inherited_attributes(&mut doc.inner)?;
    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();
    let total = selected.len();

    for (index, position) in selected.iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(total),
                format!("setting page {}", position + 1),
            ));
        }
        let page_id = page_ids[*position];
        let page = doc.inner.get_object_mut(page_id)?.as_dict_mut()?;
        page.set("MediaBox", box_array(rect));

        // A crop box that now sticks out past the new media box would leave a
        // viewer showing nothing, or nothing at all; pull it back inside or
        // drop it when there is nothing left of it.
        if let Ok(Some(crop_box)) = page_box_on(&doc.inner, page_id, b"CropBox") {
            match crop_box.intersect(&rect) {
                Some(clamped) => {
                    let page = doc.inner.get_object_mut(page_id)?.as_dict_mut()?;
                    page.set("CropBox", box_array(clamped));
                }
                None => {
                    let page = doc.inner.get_object_mut(page_id)?.as_dict_mut()?;
                    page.remove(b"CropBox");
                }
            }
        }
    }

    Ok(())
}

/// Remove `/CropBox` from the selected pages, showing the full `/MediaBox`
/// again. This is what `--reset` runs.
pub fn reset_crop(doc: &mut Document, pages: &PageRange) -> Result<()> {
    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    materialize_inherited_attributes(&mut doc.inner)?;
    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();

    for position in selected {
        let page = doc
            .inner
            .get_object_mut(page_ids[position])?
            .as_dict_mut()?;
        page.remove(b"CropBox");
    }

    Ok(())
}

/// Resize the selected pages to `size`, scaling their content per `fit`.
///
/// The media box becomes the target size at the origin and any crop box is
/// removed; content is wrapped in `q ... cm ... Q` so the existing graphics
/// state cannot leak. Pages with `/Rotate 90` or `270` show their media box on
/// its side, so the stored box is transposed to keep the *displayed* size at
/// the requested one.
pub fn scale_to(doc: &mut Document, pages: &PageRange, size: PageSize, fit: Fit) -> Result<()> {
    scale_to_with_progress(doc, pages, size, fit, None)
}

/// [`scale_to`], reporting one progress tick per selected page.
pub fn scale_to_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    size: PageSize,
    fit: Fit,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    let Some((target_w, target_h)) = size.points() else {
        return Err(PdfError::InvalidMediaBox {
            reason: "FitImage has no fixed size; pick a4, letter, legal, or WxH".into(),
        });
    };

    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    materialize_inherited_attributes(&mut doc.inner)?;
    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();
    let total = selected.len();

    for (index, position) in selected.iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(total),
                format!("scaling page {}", position + 1),
            ));
        }
        let page_id = page_ids[*position];
        let media = page_box(&doc.inner, page_id, b"MediaBox")?.ok_or_else(|| {
            PdfError::InvalidMediaBox {
                reason: format!("page {} has no /MediaBox", position + 1),
            }
        })?;
        if media.width() <= 0.0 || media.height() <= 0.0 {
            return Err(PdfError::InvalidMediaBox {
                reason: format!("page {} has a degenerate /MediaBox", position + 1),
            });
        }

        let rotation = page_rotation(&doc.inner, page_id);
        // The viewer turns a 90/270 page on its side, so store the transpose
        // to keep the displayed page at the requested size.
        let (stored_w, stored_h) = if rotation == 90 || rotation == 270 {
            (target_h, target_w)
        } else {
            (target_w, target_h)
        };

        let (scale_x, scale_y, offset_x, offset_y) = placement(&media, stored_w, stored_h, fit);

        let content = doc.inner.get_page_content(page_id);
        let decoded = Content::decode(&content).map_err(|source| {
            PdfError::Internal(format!(
                "could not decode page {} content: {source}",
                position + 1
            ))
        })?;

        let mut operations = Vec::with_capacity(decoded.operations.len() + 3);
        operations.push(Operation::new("q", vec![]));
        operations.push(Operation::new(
            "cm",
            vec![
                Object::Real(scale_x),
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(scale_y),
                Object::Real(offset_x - scale_x * media.x0),
                Object::Real(offset_y - scale_y * media.y0),
            ],
        ));
        operations.extend(decoded.operations);
        operations.push(Operation::new("Q", vec![]));

        let encoded = Content { operations }.encode().map_err(|source| {
            PdfError::Internal(format!(
                "could not encode page {} content: {source}",
                position + 1
            ))
        })?;
        doc.inner.change_page_content(page_id, encoded)?;

        let page = doc.inner.get_object_mut(page_id)?.as_dict_mut()?;
        page.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(stored_w),
                Object::Real(stored_h),
            ]),
        );
        page.remove(b"CropBox");
    }

    Ok(())
}

/// Scale and offset mapping `source` onto a `target_w` by `target_h` box.
fn placement(source: &Rect, target_w: f32, target_h: f32, fit: Fit) -> (f32, f32, f32, f32) {
    let (scale_x, scale_y) = match fit {
        Fit::Stretch => (target_w / source.width(), target_h / source.height()),
        Fit::Contain | Fit::Cover => {
            let select = match fit {
                Fit::Contain => f32::min,
                _ => f32::max,
            };
            let scale = select(target_w / source.width(), target_h / source.height());
            (scale, scale)
        }
    };

    let offset_x = (target_w - source.width() * scale_x) / 2.0;
    let offset_y = (target_h - source.height() * scale_y) / 2.0;
    (scale_x, scale_y, offset_x, offset_y)
}

/// A page's `/Rotate`, normalised into `0..360`.
fn page_rotation(doc: &lopdf::Document, page_id: ObjectId) -> i32 {
    doc.get_dictionary(page_id)
        .ok()
        .and_then(|dict| dict.get(b"Rotate").ok())
        .and_then(|object| object.as_i64().ok())
        .unwrap_or(0)
        .rem_euclid(360) as i32
}

/// Read a box entry from a page, accepting integers and reals.
fn page_box(doc: &lopdf::Document, page_id: ObjectId, key: &[u8]) -> Result<Option<Rect>> {
    let Ok(dict) = doc.get_dictionary(page_id) else {
        return Ok(None);
    };
    let Ok(object) = dict.get(key) else {
        return Ok(None);
    };
    let resolved = doc
        .dereference(object)
        .map(|(_, object)| object.clone())
        .unwrap_or_else(|_| object.clone());
    array_to_rect(&resolved)
        .map(Some)
        .map_err(|reason| PdfError::InvalidMediaBox { reason })
}

/// Read a box from an already-borrowed page dictionary id without holding the
/// borrow across the call; used where the page was materialised first.
fn page_box_on(doc: &lopdf::Document, page_id: ObjectId, key: &[u8]) -> Result<Option<Rect>> {
    page_box(doc, page_id, key)
}

fn array_to_rect(object: &Object) -> std::result::Result<Rect, String> {
    let Object::Array(values) = object else {
        return Err("the box is not an array of four numbers".into());
    };
    if values.len() != 4 {
        return Err(format!("the box has {} entries, expected 4", values.len()));
    }
    let mut numbers = [0.0f32; 4];
    for (index, value) in values.iter().enumerate() {
        numbers[index] = match value {
            Object::Integer(number) => *number as f32,
            Object::Real(number) => *number,
            _ => return Err("the box entries must be numbers".into()),
        };
    }
    if !numbers.iter().all(|number| number.is_finite()) {
        return Err("the box entries must be finite numbers".into());
    }
    Ok(Rect {
        x0: numbers[0],
        y0: numbers[1],
        x1: numbers[2],
        y1: numbers[3],
    })
}

fn box_array(rect: Rect) -> Object {
    Object::Array(vec![
        Object::Real(rect.x0),
        Object::Real(rect.y0),
        Object::Real(rect.x1),
        Object::Real(rect.y1),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_sizes_are_in_points() {
        assert_eq!(PageSize::A4.points(), Some((595.276, 841.89)));
        assert_eq!(PageSize::Letter.points(), Some((612.0, 792.0)));
        assert_eq!(PageSize::Legal.points(), Some((612.0, 1008.0)));
        assert_eq!(PageSize::FitImage.points(), None);
    }

    #[test]
    fn page_size_names_parse_case_insensitively() {
        assert_eq!(PageSize::parse("a4").unwrap(), PageSize::A4);
        assert_eq!(PageSize::parse("Letter").unwrap(), PageSize::Letter);
        assert_eq!(
            PageSize::parse("595x842").unwrap(),
            PageSize::Custom {
                width: 595.0,
                height: 842.0
            }
        );
        assert!(PageSize::parse("poster").is_err());
        assert!(PageSize::parse("0x100").is_err());
    }

    #[test]
    fn fit_names_parse() {
        assert_eq!(Fit::parse("contain").unwrap(), Fit::Contain);
        assert_eq!(Fit::parse("COVER").unwrap(), Fit::Cover);
        assert!(Fit::parse("fill").is_err());
    }

    #[test]
    fn rectangles_parse_as_origin_plus_size() {
        let rect = Rect::parse_xywh("10, 20, 100, 50").unwrap();
        assert_eq!(
            rect,
            Rect {
                x0: 10.0,
                y0: 20.0,
                x1: 110.0,
                y1: 70.0
            }
        );
        assert!(Rect::parse_xywh("10,20,-5,50").is_err());
        assert!(Rect::parse_xywh("10,20,30").is_err());
    }

    #[test]
    fn intersection_clamps_to_the_page() {
        let media = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 612.0,
            y1: 792.0,
        };
        let clamped = Rect {
            x0: -10.0,
            y0: -10.0,
            x1: 100.0,
            y1: 100.0,
        }
        .intersect(&media)
        .unwrap();
        assert_eq!(clamped.x0, 0.0);
        assert_eq!(clamped.y0, 0.0);

        assert!(Rect {
            x0: 700.0,
            y0: 900.0,
            x1: 800.0,
            y1: 1000.0
        }
        .intersect(&media)
        .is_none());
    }

    #[test]
    fn contain_preserves_aspect_ratio_inside_the_target() {
        let (sx, sy, _, _) = placement(
            &Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 200.0,
                y1: 100.0,
            },
            100.0,
            100.0,
            Fit::Contain,
        );
        assert!((sx - 0.5).abs() < 0.001);
        assert!((sx - sy).abs() < 0.001);
    }

    #[test]
    fn stretch_fills_each_axis() {
        let (sx, sy, _, _) = placement(
            &Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 200.0,
                y1: 100.0,
            },
            100.0,
            100.0,
            Fit::Stretch,
        );
        assert!((sx - 0.5).abs() < 0.001);
        assert!((sy - 1.0).abs() < 0.001);
    }
}
