//! Flatten form fields and annotations into page content.
//!
//! This operation bakes the appearance of AcroForm fields and annotations
//! directly into the page, making them non-editable. Each appearance stream
//! is a Form XObject that carries its own resources, so it is drawn with a
//! single `Do` under a matrix that maps it onto the annotation's rectangle,
//! exactly as a viewer would. The annotation objects are then dropped, and the
//! document's `/AcroForm` goes with them once no widget is left.
//!
//! Annotations with no appearance stream have nothing to draw. They stay in
//! place, and are counted in [`FlattenReport::skipped`] unless they are kinds
//! that never have one (links and popups).

use std::collections::HashSet;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream};

use super::contents::{append_content, guard_content};
use crate::assemble::materialize_inherited_attributes;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

/// Prefix for the XObject names this operation adds to a page.
///
/// A page already has names like `/Im1` or `/Fm0`; a prefix nobody else uses
/// keeps the page's own content from being repointed at an appearance stream.
const FLATTEN_PREFIX: &str = "FsFl";

/// Annotation flag bits that mean "do not show this".
const FLAG_HIDDEN: i64 = 1 << 1;
const FLAG_NO_VIEW: i64 = 1 << 5;

/// What a flattening pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlattenReport {
    /// Pages that had at least one annotation or field flattened.
    pub pages_flattened: usize,
    /// Widget annotations (form fields) flattened.
    pub widgets_flattened: usize,
    /// Non-widget annotations (stamps, markup, and so on) flattened.
    pub annots_flattened: usize,
    /// Annotations that were due to be flattened but have no usable
    /// appearance stream, and so were left on the page. Links and popups,
    /// which never have one, are not counted.
    pub skipped: usize,
    /// Whether the AcroForm dictionary was removed.
    pub acroform_removed: bool,
}

/// Options controlling which annotations to flatten.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlattenOptions {
    /// If true, only flatten widget annotations (form fields). Non-widget
    /// annotations are left intact.
    pub forms_only: bool,
    /// If true, only flatten non-widget annotations. Widget annotations are
    /// left intact.
    pub annots_only: bool,
}

/// Flatten annotations and form fields on the selected pages.
///
/// For each annotation with a normal appearance (`/AP /N`), the appearance is
/// drawn into the page content and the annotation is removed from `/Annots`.
/// Where `/N` holds several states, as a checkbox does, the one named by
/// `/AS` is drawn. Annotations marked hidden are removed without being drawn.
///
/// Once no widget annotation is left on any page, `/AcroForm` is removed from
/// the catalog. Flattening only some pages, or only annotations, leaves it in
/// place.
///
/// Returns [`PdfError::XfaForm`] for dynamic XFA forms, which have no static
/// appearance to bake in, and [`PdfError::InvalidFlattenOptions`] when both
/// `forms_only` and `annots_only` are set.
pub fn flatten(
    doc: &mut Document,
    pages: &PageRange,
    options: FlattenOptions,
) -> Result<FlattenReport> {
    flatten_with_progress(doc, pages, options, None)
}

/// [`flatten`], reporting one progress tick per selected page.
pub fn flatten_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    options: FlattenOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<FlattenReport> {
    if options.forms_only && options.annots_only {
        return Err(PdfError::InvalidFlattenOptions);
    }
    if has_xfa(&doc.inner) {
        return Err(PdfError::XfaForm);
    }

    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    // Give each page its own /Resources so that adding an XObject never
    // edits a dictionary shared with pages that were not selected.
    materialize_inherited_attributes(&mut doc.inner)?;

    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();
    let mut report = FlattenReport::default();

    for (index, position) in selected.iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(selected.len()),
                format!("flattening page {}", position + 1),
            ));
        }

        let done = flatten_page(&mut doc.inner, page_ids[*position], options)?;
        if done.widgets + done.annots > 0 {
            report.pages_flattened += 1;
        }
        report.widgets_flattened += done.widgets;
        report.annots_flattened += done.annots;
        report.skipped += done.skipped;
    }

    // With every widget gone the form has nothing left to describe.
    if !options.annots_only && no_widgets_remain(&doc.inner, &page_ids) {
        report.acroform_removed = remove_acroform(&mut doc.inner);
    }

    // Drop the annotation and field objects nothing points at any more.
    if report.widgets_flattened + report.annots_flattened > 0 || report.acroform_removed {
        doc.inner.prune_objects();
    }

    Ok(report)
}

/// What flattening one page did.
#[derive(Debug, Default)]
struct PageOutcome {
    widgets: usize,
    annots: usize,
    skipped: usize,
}

/// A form XObject chosen as an annotation's appearance.
struct Appearance {
    id: ObjectId,
    bbox: [f32; 4],
    matrix: [f32; 6],
}

fn flatten_page(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    options: FlattenOptions,
) -> Result<PageOutcome> {
    let mut outcome = PageOutcome::default();

    let Some(entries) = page_annots(doc, page_id) else {
        return Ok(outcome);
    };
    if entries.is_empty() {
        return Ok(outcome);
    }

    let mut resources = page_resources(doc, page_id);
    let mut taken: HashSet<Vec<u8>> = resource_names(doc, &resources, b"XObject");

    let mut layer: Vec<Operation> = Vec::new();
    let mut remaining: Vec<Object> = Vec::new();
    let mut changed = false;

    for entry in entries {
        // Anything that is not a readable annotation dictionary is not ours
        // to remove.
        let Some(annot) = resolve_dictionary(doc, &entry) else {
            remaining.push(entry);
            continue;
        };

        let subtype = annot.get(b"Subtype").and_then(Object::as_name).ok();
        let is_widget = subtype == Some(b"Widget".as_slice());
        let selected = match (options.forms_only, options.annots_only) {
            (true, _) => is_widget,
            (_, true) => !is_widget,
            _ => true,
        };
        if !selected {
            remaining.push(entry);
            continue;
        }

        // Hidden annotations draw nothing; flattening discards them.
        let flags = annot.get(b"F").and_then(Object::as_i64).unwrap_or(0);
        if flags & (FLAG_HIDDEN | FLAG_NO_VIEW) != 0 {
            changed = true;
            continue;
        }

        let drawn = appearance(doc, &annot).zip(annotation_rect(&annot));
        let Some((appearance, rect)) = drawn else {
            if !matches!(subtype, Some(b"Link" | b"Popup")) {
                outcome.skipped += 1;
            }
            remaining.push(entry);
            continue;
        };

        let name = unused_name(&taken);
        taken.insert(name.clone());
        let mut xobjects = sub_dictionary(doc, &resources, b"XObject");
        xobjects.set(name.clone(), Object::Reference(appearance.id));
        resources.set("XObject", Object::Dictionary(xobjects));

        let [a, b, c, d, e, f] = placement(&appearance, rect);
        layer.push(Operation::new("q", vec![]));
        layer.push(Operation::new(
            "cm",
            [a, b, c, d, e, f].into_iter().map(Object::Real).collect(),
        ));
        layer.push(Operation::new("Do", vec![Object::Name(name)]));
        layer.push(Operation::new("Q", vec![]));

        changed = true;
        if is_widget {
            outcome.widgets += 1;
        } else {
            outcome.annots += 1;
        }
    }

    if !changed {
        return Ok(outcome);
    }

    if !layer.is_empty() {
        let encoded = Content { operations: layer }.encode().map_err(|source| {
            PdfError::Internal(format!("could not encode annotation content: {source}"))
        })?;
        let layer_id = doc.add_object(Stream::new(Dictionary::new(), encoded));

        guard_content(doc, page_id)?;
        append_content(doc, page_id, layer_id)?;
    }

    let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
    page.set("Resources", Object::Dictionary(resources));
    if remaining.is_empty() {
        page.remove(b"Annots");
    } else {
        page.set("Annots", Object::Array(remaining));
    }

    Ok(outcome)
}

/// The matrix that maps an appearance onto the annotation rectangle, as
/// described in the PDF specification (12.5.5): transform the appearance's
/// bounding box by its own `/Matrix`, then scale and shift that box to the
/// rectangle. The `Do` applies `/Matrix` itself, so only the second step is
/// returned.
fn placement(appearance: &Appearance, rect: [f32; 4]) -> [f32; 6] {
    let [bx0, by0, bx1, by1] = appearance.bbox;
    let [ma, mb, mc, md, me, mf] = appearance.matrix;

    let corners = [(bx0, by0), (bx1, by0), (bx0, by1), (bx1, by1)];
    let moved = corners.map(|(x, y)| (ma * x + mc * y + me, mb * x + md * y + mf));
    let min_x = moved.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    let max_x = moved.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
    let min_y = moved.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let max_y = moved.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);

    let scale = |target: f32, source: f32| {
        if source.abs() > f32::EPSILON {
            target / source
        } else {
            1.0
        }
    };
    let sx = scale(rect[2] - rect[0], max_x - min_x);
    let sy = scale(rect[3] - rect[1], max_y - min_y);

    [sx, 0.0, 0.0, sy, rect[0] - min_x * sx, rect[1] - min_y * sy]
}

/// The normal appearance of an annotation, if it has a usable one.
fn appearance(doc: &lopdf::Document, annot: &Dictionary) -> Option<Appearance> {
    let ap = resolve_dictionary(doc, annot.get(b"AP").ok()?)?;
    let normal = ap.get(b"N").ok()?;

    // A single stream, or a dictionary of named states (a checkbox's /Yes and
    // /Off) where /AS says which one is showing.
    let id = match normal {
        Object::Reference(id) => match doc.get_object(*id).ok()? {
            Object::Stream(_) => *id,
            Object::Dictionary(states) => state_stream(annot, states)?,
            _ => return None,
        },
        Object::Dictionary(states) => state_stream(annot, states)?,
        _ => return None,
    };

    let stream = doc.get_object(id).ok()?.as_stream().ok()?;
    let bbox = number_array::<4>(doc, stream.dict.get(b"BBox").ok()?)?;
    let matrix = stream
        .dict
        .get(b"Matrix")
        .ok()
        .and_then(|matrix| number_array::<6>(doc, matrix))
        .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    Some(Appearance { id, bbox, matrix })
}

fn state_stream(annot: &Dictionary, states: &Dictionary) -> Option<ObjectId> {
    let state = annot.get(b"AS").and_then(Object::as_name).ok()?;
    states.get(state).ok()?.as_reference().ok()
}

/// The annotation's `/Rect`, ordered so that the first corner is lower left.
fn annotation_rect(annot: &Dictionary) -> Option<[f32; 4]> {
    let values = annot.get(b"Rect").ok()?.as_array().ok()?;
    let [x0, y0, x1, y1] = numbers(values)?;
    Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
}

fn numbers<const N: usize>(values: &[Object]) -> Option<[f32; N]> {
    if values.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = match value {
            Object::Integer(number) => *number as f32,
            Object::Real(number) => *number,
            _ => return None,
        };
    }
    out.iter().all(|number| number.is_finite()).then_some(out)
}

fn number_array<const N: usize>(doc: &lopdf::Document, object: &Object) -> Option<[f32; N]> {
    let (_, resolved) = doc.dereference(object).ok()?;
    numbers(resolved.as_array().ok()?)
}

/// Whether the document carries a dynamic XFA form.
fn has_xfa(doc: &lopdf::Document) -> bool {
    catalog(doc)
        .and_then(|catalog| catalog.get(b"AcroForm").ok())
        .and_then(|form| resolve_dictionary(doc, form))
        .is_some_and(|form| form.get(b"XFA").is_ok())
}

fn catalog(doc: &lopdf::Document) -> Option<&Dictionary> {
    let root = doc.trailer.get(b"Root").ok()?.as_reference().ok()?;
    doc.get_dictionary(root).ok()
}

/// True when no page in the document still has a widget annotation.
fn no_widgets_remain(doc: &lopdf::Document, page_ids: &[ObjectId]) -> bool {
    page_ids.iter().all(|page_id| {
        page_annots(doc, *page_id).is_none_or(|entries| {
            entries.iter().all(|entry| {
                resolve_dictionary(doc, entry)
                    .and_then(|annot| {
                        annot
                            .get(b"Subtype")
                            .and_then(Object::as_name)
                            .ok()
                            .map(<[u8]>::to_vec)
                    })
                    .is_none_or(|subtype| subtype != b"Widget")
            })
        })
    })
}

/// Remove `/AcroForm` from the catalog. Returns whether there was one.
fn remove_acroform(doc: &mut lopdf::Document) -> bool {
    let Some(root) = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| root.as_reference().ok())
    else {
        return false;
    };
    doc.get_object_mut(root)
        .and_then(Object::as_dict_mut)
        .is_ok_and(|catalog| catalog.remove(b"AcroForm").is_some())
}

/// A page's `/Annots` entries, whether the array is direct or referenced.
fn page_annots(doc: &lopdf::Document, page_id: ObjectId) -> Option<Vec<Object>> {
    let page = doc.get_dictionary(page_id).ok()?;
    let annots = page.get(b"Annots").ok()?;
    let (_, resolved) = doc.dereference(annots).ok()?;
    resolved.as_array().ok().cloned()
}

/// A copy of the page's own `/Resources`.
fn page_resources(doc: &lopdf::Document, page_id: ObjectId) -> Dictionary {
    doc.get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Resources").ok())
        .and_then(|resources| resolve_dictionary(doc, resources))
        .unwrap_or_default()
}

/// A copy of one category of a resource dictionary, such as `/XObject`.
fn sub_dictionary(doc: &lopdf::Document, resources: &Dictionary, key: &[u8]) -> Dictionary {
    resources
        .get(key)
        .ok()
        .and_then(|entry| resolve_dictionary(doc, entry))
        .unwrap_or_default()
}

fn resource_names(doc: &lopdf::Document, resources: &Dictionary, key: &[u8]) -> HashSet<Vec<u8>> {
    sub_dictionary(doc, resources, key)
        .iter()
        .map(|(name, _)| name.clone())
        .collect()
}

/// The next `FsFl<n>` not already used as an XObject name.
fn unused_name(taken: &HashSet<Vec<u8>>) -> Vec<u8> {
    (0usize..)
        .map(|n| format!("{FLATTEN_PREFIX}{n}").into_bytes())
        .find(|name| !taken.contains(name))
        .expect("an unbounded range always yields a free name")
}

/// Resolve a dictionary object (direct or reference).
fn resolve_dictionary(doc: &lopdf::Document, object: &Object) -> Option<Dictionary> {
    match object {
        Object::Dictionary(dict) => Some(dict.clone()),
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn appearance(bbox: [f32; 4], matrix: [f32; 6]) -> Appearance {
        Appearance {
            id: (1, 0),
            bbox,
            matrix,
        }
    }

    const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    #[test]
    fn numbers_reads_integers_and_reals() {
        let values = [
            Object::Integer(1),
            Object::Real(2.5),
            Object::Integer(3),
            Object::Real(4.0),
        ];
        assert_eq!(numbers::<4>(&values), Some([1.0, 2.5, 3.0, 4.0]));
    }

    #[test]
    fn numbers_rejects_wrong_length_and_wrong_types() {
        assert!(numbers::<4>(&[Object::Integer(1), Object::Integer(2)]).is_none());
        let values = [
            Object::Integer(1),
            Object::Name(b"x".to_vec()),
            Object::Integer(3),
            Object::Integer(4),
        ];
        assert!(numbers::<4>(&values).is_none());
    }

    #[test]
    fn an_appearance_is_moved_and_scaled_onto_the_rectangle() {
        // A 50x20 box at the origin, shown in a 100x40 rectangle at (200, 300).
        let matrix = placement(
            &appearance([0.0, 0.0, 50.0, 20.0], IDENTITY),
            [200.0, 300.0, 300.0, 340.0],
        );
        assert_eq!(matrix, [2.0, 0.0, 0.0, 2.0, 200.0, 300.0]);
    }

    #[test]
    fn a_bounding_box_that_does_not_start_at_the_origin_is_offset() {
        let matrix = placement(
            &appearance([10.0, 10.0, 60.0, 30.0], IDENTITY),
            [0.0, 0.0, 50.0, 20.0],
        );
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, -10.0, -10.0]);
    }

    #[test]
    fn the_appearance_matrix_is_taken_into_account() {
        // A quarter turn: the 50x20 box becomes 20x50 once /Matrix is applied.
        let turned = [0.0, 1.0, -1.0, 0.0, 0.0, 0.0];
        let matrix = placement(
            &appearance([0.0, 0.0, 50.0, 20.0], turned),
            [0.0, 0.0, 20.0, 50.0],
        );
        // The transformed box spans x in [-20, 0], y in [0, 50].
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, 20.0, 0.0]);
    }

    #[test]
    fn a_flat_bounding_box_does_not_divide_by_zero() {
        let matrix = placement(
            &appearance([0.0, 0.0, 0.0, 0.0], IDENTITY),
            [5.0, 5.0, 10.0, 10.0],
        );
        assert!(matrix.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn the_rectangle_is_put_in_order() {
        let mut annot = Dictionary::new();
        annot.set(
            "Rect",
            Object::Array(vec![
                Object::Integer(100),
                Object::Integer(200),
                Object::Integer(10),
                Object::Integer(20),
            ]),
        );
        assert_eq!(annotation_rect(&annot), Some([10.0, 20.0, 100.0, 200.0]));
    }

    #[test]
    fn generated_names_skip_ones_in_use() {
        let taken: HashSet<Vec<u8>> = [b"FsFl0".to_vec(), b"FsFl1".to_vec()].into_iter().collect();
        assert_eq!(unused_name(&taken), b"FsFl2".to_vec());
    }

    #[test]
    fn report_and_options_default_to_nothing() {
        assert_eq!(FlattenReport::default().skipped, 0);
        assert!(!FlattenReport::default().acroform_removed);
        assert_eq!(
            FlattenOptions::default(),
            FlattenOptions {
                forms_only: false,
                annots_only: false
            }
        );
    }
}
