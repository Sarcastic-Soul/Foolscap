//! Working out how large an image is actually drawn.
//!
//! A PDF image XObject carries pixel dimensions but says nothing about the size
//! it appears at. That comes from the current transformation matrix at the
//! moment the content stream invokes it. Resampling without this is guesswork:
//! a 4000-pixel-wide photo placed in a 2-inch box is wildly oversampled, while
//! the same photo across a full page is not.
//!
//! So this module walks each page's content stream, tracks the graphics state
//! stack, and records the largest box each image is drawn into.

use std::collections::HashMap;

use lopdf::content::Content;
use lopdf::{Dictionary, Object, ObjectId};

/// Depth limit for nested Form XObjects, which may recurse.
const MAX_FORM_DEPTH: usize = 8;

/// The largest size, in PDF points, that an image is drawn at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub width: f32,
    pub height: f32,
}

impl Placement {
    /// The resolution the image is effectively displayed at, given its pixel
    /// dimensions. Returns `None` when the placement is degenerate.
    pub(crate) fn effective_dpi(&self, pixel_width: u32, pixel_height: u32) -> Option<f32> {
        if self.width <= 0.0 || self.height <= 0.0 {
            return None;
        }

        let horizontal = pixel_width as f32 / (self.width / 72.0);
        let vertical = pixel_height as f32 / (self.height / 72.0);
        let dpi = horizontal.max(vertical);

        dpi.is_finite().then_some(dpi)
    }

    fn merge(&mut self, other: Placement) {
        self.width = self.width.max(other.width);
        self.height = self.height.max(other.height);
    }
}

/// A 2D affine transform, in the `[a b c d e f]` order PDF writes it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Matrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Matrix {
    const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// `self` applied after `other`, which is the order `cm` composes in.
    fn concat(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// An image occupies the unit square, so the transformed edge lengths are
    /// the size it is drawn at. Rotation and skew are handled by taking the
    /// length of each transformed basis vector.
    fn unit_square_size(&self) -> Placement {
        Placement {
            width: self.a.hypot(self.b),
            height: self.c.hypot(self.d),
        }
    }
}

/// Find the largest drawn size of every image in the document.
///
/// Images that are never drawn do not appear in the result; nothing can be
/// concluded about their required resolution, so callers should leave them be.
pub(crate) fn measure_images(doc: &lopdf::Document) -> HashMap<ObjectId, Placement> {
    let mut placements = HashMap::new();

    for page_id in doc.get_pages().into_values() {
        let Ok(content) = doc.get_page_content(page_id) else {
            continue;
        };
        let Ok(decoded) = Content::decode(&content) else {
            tracing::debug!(?page_id, "could not decode page content stream");
            continue;
        };

        let resources = resources_for_page(doc, page_id);
        walk(
            doc,
            &decoded,
            resources.as_ref(),
            Matrix::IDENTITY,
            0,
            &mut placements,
        );
    }

    placements
}

/// Resolve a page's `/Resources`, following the page tree upwards since it is
/// an inheritable attribute.
fn resources_for_page(doc: &lopdf::Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut current = Some(page_id);

    for _ in 0..64 {
        let id = current?;
        let dict = doc.get_dictionary(id).ok()?;

        if let Ok(resources) = dict.get(b"Resources") {
            return resolve_dictionary(doc, resources);
        }

        current = dict
            .get(b"Parent")
            .ok()
            .and_then(|object| object.as_reference().ok());
    }

    None
}

fn resolve_dictionary(doc: &lopdf::Document, object: &Object) -> Option<Dictionary> {
    match object {
        Object::Dictionary(dict) => Some(dict.clone()),
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        _ => None,
    }
}

/// Interpret a content stream, tracking the transformation matrix.
fn walk(
    doc: &lopdf::Document,
    content: &Content,
    resources: Option<&Dictionary>,
    initial: Matrix,
    depth: usize,
    placements: &mut HashMap<ObjectId, Placement>,
) {
    let mut ctm = initial;
    let mut stack: Vec<Matrix> = Vec::new();

    for operation in &content.operations {
        match operation.operator.as_str() {
            "q" => stack.push(ctm),
            "Q" => ctm = stack.pop().unwrap_or(initial),
            "cm" => {
                if let Some(matrix) = matrix_from(&operation.operands) {
                    ctm = matrix.concat(&ctm);
                }
            }
            "Do" => {
                let Some(name) = operation.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some((id, dict)) = lookup_xobject(doc, resources, name) else {
                    continue;
                };

                match dict.get(b"Subtype").and_then(Object::as_name_str).ok() {
                    Some("Image") => {
                        let placement = ctm.unit_square_size();
                        placements
                            .entry(id)
                            .and_modify(|existing| existing.merge(placement))
                            .or_insert(placement);
                    }
                    Some("Form") if depth < MAX_FORM_DEPTH => {
                        walk_form(doc, id, &dict, resources, ctm, depth, placements);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Descend into a Form XObject, which carries its own optional matrix and
/// resource dictionary.
fn walk_form(
    doc: &lopdf::Document,
    id: ObjectId,
    dict: &Dictionary,
    outer_resources: Option<&Dictionary>,
    ctm: Matrix,
    depth: usize,
    placements: &mut HashMap<ObjectId, Placement>,
) {
    let Ok(Object::Stream(stream)) = doc.get_object(id) else {
        return;
    };
    let Ok(bytes) = stream.decompressed_content() else {
        return;
    };
    let Ok(decoded) = Content::decode(&bytes) else {
        return;
    };

    let inner_ctm = dict
        .get(b"Matrix")
        .ok()
        .and_then(|object| object.as_array().ok())
        .and_then(|values| matrix_from(values))
        .map(|matrix| matrix.concat(&ctm))
        .unwrap_or(ctm);

    // A form's own resources win; where it has none it inherits the page's.
    let inner_resources = dict
        .get(b"Resources")
        .ok()
        .and_then(|object| resolve_dictionary(doc, object));

    walk(
        doc,
        &decoded,
        inner_resources.as_ref().or(outer_resources),
        inner_ctm,
        depth + 1,
        placements,
    );
}

/// Resolve an XObject name against a resource dictionary.
fn lookup_xobject(
    doc: &lopdf::Document,
    resources: Option<&Dictionary>,
    name: &[u8],
) -> Option<(ObjectId, Dictionary)> {
    let xobjects = resolve_dictionary(doc, resources?.get(b"XObject").ok()?)?;
    let id = xobjects.get(name).ok()?.as_reference().ok()?;

    let dict = match doc.get_object(id).ok()? {
        Object::Stream(stream) => stream.dict.clone(),
        Object::Dictionary(dict) => dict.clone(),
        _ => return None,
    };

    Some((id, dict))
}

fn matrix_from(operands: &[Object]) -> Option<Matrix> {
    if operands.len() < 6 {
        return None;
    }

    let value = |index: usize| -> Option<f32> {
        match &operands[index] {
            Object::Integer(number) => Some(*number as f32),
            Object::Real(number) => Some(*number),
            _ => None,
        }
    };

    Some(Matrix {
        a: value(0)?,
        b: value(1)?,
        c: value(2)?,
        d: value(3)?,
        e: value(4)?,
        f: value(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scaling_matrix_gives_the_drawn_size() {
        let matrix = Matrix {
            a: 200.0,
            b: 0.0,
            c: 0.0,
            d: 100.0,
            e: 50.0,
            f: 50.0,
        };
        let placement = matrix.unit_square_size();

        assert_eq!(placement.width, 200.0);
        assert_eq!(placement.height, 100.0);
    }

    #[test]
    fn a_rotated_matrix_keeps_its_edge_lengths() {
        // 90 degrees: the image is 200 wide and 100 tall, turned on its side.
        let matrix = Matrix {
            a: 0.0,
            b: 200.0,
            c: -100.0,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        };
        let placement = matrix.unit_square_size();

        assert_eq!(placement.width, 200.0);
        assert_eq!(placement.height, 100.0);
    }

    #[test]
    fn concatenation_composes_scales() {
        let half = Matrix {
            a: 0.5,
            d: 0.5,
            ..Matrix::IDENTITY
        };
        let hundred = Matrix {
            a: 100.0,
            d: 100.0,
            ..Matrix::IDENTITY
        };

        let combined = hundred.concat(&half);
        let placement = combined.unit_square_size();

        assert_eq!(placement.width, 50.0);
        assert_eq!(placement.height, 50.0);
    }

    #[test]
    fn identity_leaves_a_matrix_alone() {
        let matrix = Matrix {
            a: 3.0,
            b: 1.0,
            c: 2.0,
            d: 4.0,
            e: 5.0,
            f: 6.0,
        };
        assert_eq!(matrix.concat(&Matrix::IDENTITY), matrix);
        assert_eq!(Matrix::IDENTITY.concat(&matrix), matrix);
    }

    #[test]
    fn effective_dpi_relates_pixels_to_points() {
        // A 100 point box is 100/72 inches; 300 pixels across it is 216 dpi.
        let placement = Placement {
            width: 100.0,
            height: 100.0,
        };
        let dpi = placement.effective_dpi(300, 300).unwrap();
        assert!((dpi - 216.0).abs() < 0.01, "got {dpi}");
    }

    #[test]
    fn effective_dpi_takes_the_more_demanding_axis() {
        let placement = Placement {
            width: 720.0,
            height: 72.0,
        };
        // 720 points is 10 inches, 72 points is 1 inch.
        let dpi = placement.effective_dpi(1000, 300).unwrap();
        assert!((dpi - 300.0).abs() < 0.01, "got {dpi}");
    }

    #[test]
    fn a_degenerate_placement_has_no_resolution() {
        let placement = Placement {
            width: 0.0,
            height: 10.0,
        };
        assert_eq!(placement.effective_dpi(100, 100), None);
    }

    #[test]
    fn merging_keeps_the_largest_placement() {
        let mut placement = Placement {
            width: 100.0,
            height: 50.0,
        };
        placement.merge(Placement {
            width: 40.0,
            height: 200.0,
        });

        assert_eq!(placement.width, 100.0);
        assert_eq!(placement.height, 200.0);
    }

    #[test]
    fn operands_that_are_not_numbers_are_rejected() {
        assert!(matrix_from(&[]).is_none());
        assert!(matrix_from(&[1.into(), 2.into()]).is_none());
        assert!(matrix_from(&[
            Object::Name(b"nope".to_vec()),
            2.into(),
            3.into(),
            4.into(),
            5.into(),
            6.into()
        ])
        .is_none());
    }
}
