//! Lossy size reduction: resampling and recompressing embedded images.
//!
//! This is where the bytes actually are in a real document. [`optimize`] is
//! lossless and typically saves single digits of a percent;
//! this pass routinely halves a scanned or photo-heavy file.
//!
//! [`optimize`]: crate::ops::optimize

pub(crate) mod predictor;

use std::collections::HashMap;

use lopdf::{Object, ObjectId};

use super::image::{build_image_stream, decode_image, encode_jpeg, is_masked, serialized_len};
use crate::document::Document;
use crate::error::Result;
use crate::ops::optimize::{optimize, OptimizeLevel, OptimizeReport};
use crate::placement::{measure_images, Placement};
use crate::progress::{Progress, ProgressFn};

/// How hard to squeeze, in the vocabulary Ghostscript established and users
/// already know.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CompressLevel {
    /// 72 dpi. For reading on a screen; not for printing.
    Screen,
    /// 150 dpi. A reasonable default: sharp on a display, adequate on paper.
    #[default]
    Ebook,
    /// 300 dpi. Print quality, so only oversampled images shrink.
    Print,
}

impl CompressLevel {
    /// Resolution to resample down to.
    pub fn target_dpi(&self) -> f32 {
        match self {
            CompressLevel::Screen => 72.0,
            CompressLevel::Ebook => 150.0,
            CompressLevel::Print => 300.0,
        }
    }

    /// JPEG quality, 1 to 100.
    pub fn jpeg_quality(&self) -> u8 {
        match self {
            CompressLevel::Screen => 60,
            CompressLevel::Ebook => 75,
            CompressLevel::Print => 88,
        }
    }
}

/// Why an image was left alone.
///
/// Worth reporting rather than hiding: a document that barely shrank should be
/// explicable, not mysterious.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// A stencil mask or an explicit `/Mask`, where recompression would corrupt
    /// the shape it cuts out.
    Mask,
    /// A filter this pass cannot decode: JPX, CCITT, JBIG2, or a Flate image
    /// whose predictor cannot be reversed (the TIFF predictor at a depth other
    /// than 8 bits, or malformed predictor parameters).
    UnsupportedFilter,
    /// A colour space this pass cannot interpret, such as Indexed or Separation.
    UnsupportedColorSpace,
    /// Never drawn by any content stream, so there is no basis for choosing a
    /// resolution.
    NeverDrawn,
    /// Already at or below the target resolution.
    AlreadySmall,
    /// Recompression did not actually save anything.
    NoSaving,
    /// Fewer pixels than it is worth touching; recompressing an icon costs
    /// quality and saves nothing.
    Tiny,
}

/// What a compression pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompressReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub images_examined: usize,
    pub images_recompressed: usize,
    pub image_bytes_before: u64,
    pub image_bytes_after: u64,
    pub skipped: HashMap<SkipReason, usize>,
    /// The lossless pass that runs afterwards.
    pub optimize: OptimizeReport,
}

impl CompressReport {
    pub fn bytes_saved(&self) -> u64 {
        self.bytes_before.saturating_sub(self.bytes_after)
    }

    pub fn ratio_saved(&self) -> f64 {
        if self.bytes_before == 0 {
            return 0.0;
        }
        self.bytes_saved() as f64 / self.bytes_before as f64
    }

    pub fn images_skipped(&self) -> usize {
        self.skipped.values().sum()
    }

    fn skip(&mut self, reason: SkipReason) {
        *self.skipped.entry(reason).or_insert(0) += 1;
    }
}

/// Images below this many pixels on their longest edge are left alone.
const TINY_EDGE: u32 = 64;

/// How far above the target an image has to be before resampling is worth the
/// quality cost. Without a margin, a 152 dpi image would be resampled to 150.
const RESAMPLE_MARGIN: f32 = 1.15;

/// Resample and recompress the document's images, then run a lossless pass.
pub fn compress(doc: &mut Document, level: CompressLevel) -> Result<CompressReport> {
    compress_with_progress(doc, level, None)
}

/// [`compress`], reporting one progress tick per image examined.
pub fn compress_with_progress(
    doc: &mut Document,
    level: CompressLevel,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<CompressReport> {
    let mut report = CompressReport {
        bytes_before: serialized_len(doc)?,
        ..Default::default()
    };

    let placements = measure_images(&doc.inner);
    let image_ids = collect_image_ids(&doc.inner);
    let total = image_ids.len();
    report.images_examined = total;

    for (index, id) in image_ids.into_iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(total),
                format!("image {} of {total}", index + 1),
            ));
        }

        match recompress_one(&mut doc.inner, id, placements.get(&id), level) {
            Ok(Outcome::Replaced { before, after }) => {
                report.images_recompressed += 1;
                report.image_bytes_before += before;
                report.image_bytes_after += after;
            }
            Ok(Outcome::Skipped(reason)) => report.skip(reason),
            Err(reason) => {
                tracing::debug!(?id, ?reason, "image left untouched");
                report.skip(reason);
            }
        }
    }

    report.optimize = optimize(doc, OptimizeLevel::Aggressive)?;
    report.bytes_after = serialized_len(doc)?;

    tracing::debug!(
        before = report.bytes_before,
        after = report.bytes_after,
        recompressed = report.images_recompressed,
        skipped = report.images_skipped(),
        "compress pass complete"
    );

    Ok(report)
}

enum Outcome {
    Replaced { before: u64, after: u64 },
    Skipped(SkipReason),
}

fn collect_image_ids(doc: &lopdf::Document) -> Vec<ObjectId> {
    doc.objects
        .iter()
        .filter_map(|(id, object)| {
            let Object::Stream(stream) = object else {
                return None;
            };
            let subtype = stream.dict.get(b"Subtype").ok()?.as_name().ok()?;
            (subtype == b"Image").then_some(*id)
        })
        .collect()
}

fn recompress_one(
    doc: &mut lopdf::Document,
    id: ObjectId,
    placement: Option<&Placement>,
    level: CompressLevel,
) -> std::result::Result<Outcome, SkipReason> {
    let stream = match doc.get_object(id) {
        Ok(Object::Stream(stream)) => stream.clone(),
        _ => return Err(SkipReason::UnsupportedFilter),
    };

    if is_masked(&stream.dict) {
        return Ok(Outcome::Skipped(SkipReason::Mask));
    }

    let decoded = decode_image(doc, &stream)?;
    let (width, height) = (decoded.width(), decoded.height());

    if width.max(height) <= TINY_EDGE {
        return Ok(Outcome::Skipped(SkipReason::Tiny));
    }

    let Some(placement) = placement else {
        return Ok(Outcome::Skipped(SkipReason::NeverDrawn));
    };
    let Some(current_dpi) = placement.effective_dpi(width, height) else {
        return Ok(Outcome::Skipped(SkipReason::NeverDrawn));
    };

    let target_dpi = level.target_dpi();
    if current_dpi <= target_dpi * RESAMPLE_MARGIN {
        return Ok(Outcome::Skipped(SkipReason::AlreadySmall));
    }

    let factor = target_dpi / current_dpi;
    let new_width = ((width as f32 * factor).round() as u32).max(1);
    let new_height = ((height as f32 * factor).round() as u32).max(1);

    let resized = decoded.resize_exact(new_width, new_height, image::imageops::Lanczos3);
    let encoded = encode_jpeg(&resized, level.jpeg_quality())?;

    let before = stream.content.len() as u64;
    let after = encoded.len() as u64;

    if after >= before {
        return Ok(Outcome::Skipped(SkipReason::NoSaving));
    }

    let replacement = build_image_stream(&stream.dict, &resized, encoded);
    doc.set_object(id, Object::Stream(replacement));

    tracing::debug!(
        ?id,
        from = format!("{width}x{height}"),
        to = format!("{new_width}x{new_height}"),
        dpi = current_dpi,
        "resampled image"
    );

    Ok(Outcome::Replaced { before, after })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Dictionary;

    #[test]
    fn levels_target_the_resolutions_their_names_imply() {
        assert_eq!(CompressLevel::Screen.target_dpi(), 72.0);
        assert_eq!(CompressLevel::Ebook.target_dpi(), 150.0);
        assert_eq!(CompressLevel::Print.target_dpi(), 300.0);
        assert_eq!(CompressLevel::default(), CompressLevel::Ebook);
    }

    #[test]
    fn quality_rises_with_the_target_resolution() {
        assert!(CompressLevel::Screen.jpeg_quality() < CompressLevel::Ebook.jpeg_quality());
        assert!(CompressLevel::Ebook.jpeg_quality() < CompressLevel::Print.jpeg_quality());
    }

    #[test]
    fn a_stencil_mask_is_recognised() {
        let mut dict = Dictionary::new();
        assert!(!is_masked(&dict));

        dict.set("ImageMask", Object::Boolean(true));
        assert!(is_masked(&dict));
    }

    #[test]
    fn a_soft_mask_is_not_treated_as_a_mask() {
        // /SMask is a separate object and survives the base image changing.
        let mut dict = Dictionary::new();
        dict.set("SMask", Object::Reference((7, 0)));
        assert!(!is_masked(&dict));
    }

    #[test]
    fn an_explicit_mask_is_left_alone() {
        let mut dict = Dictionary::new();
        dict.set("Mask", Object::Reference((7, 0)));
        assert!(is_masked(&dict));
    }

    #[test]
    fn the_report_counts_skips_by_reason() {
        let mut report = CompressReport::default();
        report.skip(SkipReason::Tiny);
        report.skip(SkipReason::Tiny);
        report.skip(SkipReason::Mask);

        assert_eq!(report.images_skipped(), 3);
        assert_eq!(report.skipped[&SkipReason::Tiny], 2);
    }

    #[test]
    fn a_larger_output_never_reports_a_saving() {
        let report = CompressReport {
            bytes_before: 100,
            bytes_after: 200,
            ..Default::default()
        };
        assert_eq!(report.bytes_saved(), 0);
        assert_eq!(report.ratio_saved(), 0.0);
    }
}
