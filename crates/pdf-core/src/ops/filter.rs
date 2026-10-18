//! Whole-image colour filters: inversion and greyscale.
//!
//! Each image XObject drawn by the selected pages is decoded with the same
//! path [`compress`](super::compress) uses, transformed pixel by pixel, and
//! spliced back as JPEG. Images that path cannot decode (masks, exotic
//! filters or colour spaces) are counted in the report and left alone.
//!
//! Only raster images change. Vector text and line art stay exactly as they
//! were, so filtering a page inverts its photos but not its letters. A
//! `--raster` fallback that renders the whole page first would cover vectors
//! too; that is deliberately not done here.

use std::collections::{HashMap, HashSet};

use lopdf::content::Content;
use lopdf::{Dictionary, Object, ObjectId};

use super::compress::SkipReason;
use super::image::{
    build_gray_image_stream, build_image_stream, decode_image, encode_gray_jpeg, encode_jpeg,
    is_masked,
};
use crate::assemble::materialize_inherited_attributes;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};

/// Depth limit for nested Form XObjects, which may recurse.
const MAX_FORM_DEPTH: usize = 8;

/// JPEG quality for the re-encoded images. The transform itself is lossless;
/// the container is not, so encode near the top to avoid adding artefacts.
const FILTER_JPEG_QUALITY: u8 = 92;

/// Which per-pixel transform to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Flip every sample. Useful for dark-mode reading of scanned pages.
    Invert,
    /// Drop the colour, keeping the luminance.
    Grayscale,
}

impl Filter {
    /// Parse `invert`/`inverse` or `grayscale`/`grey`/`gray`.
    pub fn parse(spec: &str) -> Result<Self> {
        match spec.trim().to_ascii_lowercase().as_str() {
            "invert" | "inverse" | "negate" => Ok(Filter::Invert),
            "grayscale" | "greyscale" | "gray" | "grey" => Ok(Filter::Grayscale),
            _ => Err(PdfError::InvalidFilter(spec.to_string())),
        }
    }
}

/// What a filter pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilterReport {
    pub images_examined: usize,
    pub images_filtered: usize,
    pub skipped: HashMap<SkipReason, usize>,
}

impl FilterReport {
    pub fn images_skipped(&self) -> usize {
        self.skipped.values().sum()
    }

    fn skip(&mut self, reason: SkipReason) {
        *self.skipped.entry(reason).or_insert(0) += 1;
    }
}

/// Apply `filter` to the images drawn by `pages`.
pub fn filter(doc: &mut Document, pages: &PageRange, filter: Filter) -> Result<FilterReport> {
    filter_with_progress(doc, pages, filter, None)
}

/// [`filter`], reporting one progress tick per image examined.
pub fn filter_with_progress(
    doc: &mut Document,
    pages: &PageRange,
    filter: Filter,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<FilterReport> {
    let selected = pages.resolve_unique(doc.page_count())?;
    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    materialize_inherited_attributes(&mut doc.inner)?;

    let targets = images_on_pages(&doc.inner, &selected);
    let mut report = FilterReport {
        images_examined: targets.len(),
        ..Default::default()
    };

    for (index, id) in targets.into_iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(report.images_examined),
                format!("filtering image {}", index + 1),
            ));
        }

        match apply_one(&mut doc.inner, id, filter) {
            Ok(true) => report.images_filtered += 1,
            Ok(false) => {}
            Err(reason) => {
                tracing::debug!(?id, ?reason, "image left untouched");
                report.skip(reason);
            }
        }
    }

    Ok(report)
}

fn apply_one(
    doc: &mut lopdf::Document,
    id: ObjectId,
    filter: Filter,
) -> std::result::Result<bool, SkipReason> {
    let stream = match doc.get_object(id) {
        Ok(Object::Stream(stream)) => stream.clone(),
        _ => return Err(SkipReason::UnsupportedFilter),
    };

    if is_masked(&stream.dict) {
        return Err(SkipReason::Mask);
    }

    let decoded = decode_image(doc, &stream)?;

    let replacement = match filter {
        Filter::Invert => {
            let mut pixels = decoded.to_rgb8();
            image::imageops::invert(&mut pixels);
            let image = image::DynamicImage::ImageRgb8(pixels);
            let encoded = encode_jpeg(&image, FILTER_JPEG_QUALITY)?;
            build_image_stream(&stream.dict, &image, encoded)
        }
        Filter::Grayscale => {
            let gray = decoded.to_luma8();
            let encoded = encode_gray_jpeg(&gray, FILTER_JPEG_QUALITY)?;
            build_gray_image_stream(&stream.dict, &gray, encoded)
        }
    };

    // A pathological re-encode that grows the stream still applies: unlike
    // compression this pass is about appearance, not size.
    doc.set_object(id, Object::Stream(replacement));
    Ok(true)
}

/// Every image XObject invoked, directly or through Form XObjects, by the
/// selected pages.
fn images_on_pages(doc: &lopdf::Document, selected: &[usize]) -> Vec<ObjectId> {
    let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
    let mut found = Vec::new();
    let mut seen = HashSet::new();

    for position in selected {
        let Some(page_id) = page_ids.get(*position).copied() else {
            continue;
        };
        let content = doc.get_page_content(page_id);
        let Ok(decoded) = Content::decode(&content) else {
            continue;
        };
        let resources = resources_for_page(doc, page_id);
        collect(doc, &decoded, resources.as_ref(), 0, &mut found, &mut seen);
    }

    found
}

fn collect(
    doc: &lopdf::Document,
    content: &Content,
    resources: Option<&Dictionary>,
    depth: usize,
    found: &mut Vec<ObjectId>,
    seen: &mut HashSet<ObjectId>,
) {
    for operation in &content.operations {
        if operation.operator != "Do" {
            continue;
        }
        let Some(name) = operation.operands.first().and_then(|o| o.as_name().ok()) else {
            continue;
        };
        let Some((id, dict)) = lookup_xobject(doc, resources, name) else {
            continue;
        };
        match dict.get(b"Subtype").and_then(Object::as_name).ok() {
            Some(b"Image") => {
                if seen.insert(id) {
                    found.push(id);
                }
            }
            Some(b"Form") if depth < MAX_FORM_DEPTH => {
                if let Ok(Object::Stream(stream)) = doc.get_object(id) {
                    let Ok(bytes) = stream.decompressed_content() else {
                        continue;
                    };
                    let Ok(decoded) = Content::decode(&bytes) else {
                        continue;
                    };
                    let inner = dict
                        .get(b"Resources")
                        .ok()
                        .and_then(|object| resolve_dictionary(doc, object));
                    collect(
                        doc,
                        &decoded,
                        inner.as_ref().or(resources),
                        depth + 1,
                        found,
                        seen,
                    );
                }
            }
            _ => {}
        }
    }
}

/// A page's `/Resources`, following the page tree upwards.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_names_parse() {
        assert_eq!(Filter::parse("invert").unwrap(), Filter::Invert);
        assert_eq!(Filter::parse("Grayscale").unwrap(), Filter::Grayscale);
        assert_eq!(Filter::parse("grey").unwrap(), Filter::Grayscale);
        assert!(Filter::parse("blur").is_err());
    }

    #[test]
    fn the_report_counts_skips() {
        let mut report = FilterReport::default();
        assert_eq!(report.images_skipped(), 0);
        report.skip(SkipReason::Mask);
        assert_eq!(report.images_skipped(), 1);
    }
}
