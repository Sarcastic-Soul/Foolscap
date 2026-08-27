//! Images to PDF, and PDF to images.

use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageReader};
use printpdf::{Mm, Op, PdfDocument, PdfPage, PdfSaveOptions, Pt, RawImage, XObjectTransform};

use crate::error::{PdfError, Result};
use crate::progress::{Progress, ProgressFn};

/// Standard page sizes, in PDF points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageSize {
    A4,
    Letter,
    Legal,
    /// One page per image, exactly the size of that image at `dpi`. Nothing is
    /// cropped and nothing is letterboxed.
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
}

/// How an image is placed on a page larger or smaller than itself.
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

/// Options for [`images_to_pdf`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImagesToPdfOptions {
    pub page_size: PageSize,
    pub fit: Fit,
    /// Blank border in points, applied on every side. Ignored for
    /// [`PageSize::FitImage`].
    pub margin: f32,
    /// Assumed resolution when turning image pixels into page points, used only
    /// by [`PageSize::FitImage`].
    pub dpi: f32,
}

impl Default for ImagesToPdfOptions {
    fn default() -> Self {
        Self {
            page_size: PageSize::A4,
            fit: Fit::Contain,
            margin: 0.0,
            dpi: 300.0,
        }
    }
}

/// Where an image ends up on its page, in points from the bottom left.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    page_width: f32,
    page_height: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// Build a PDF with one page per image.
pub fn images_to_pdf(inputs: &[PathBuf], output: &Path, options: ImagesToPdfOptions) -> Result<()> {
    images_to_pdf_with_progress(inputs, output, options, None)
}

/// [`images_to_pdf`], reporting one progress tick per image.
pub fn images_to_pdf_with_progress(
    inputs: &[PathBuf],
    output: &Path,
    options: ImagesToPdfOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<()> {
    if inputs.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    let total = inputs.len();
    let mut document = PdfDocument::new("Foolscap");
    let mut pages = Vec::with_capacity(total);

    for (index, path) in inputs.iter().enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(total),
                format!("reading {}", path.display()),
            ));
        }

        let decoded = load_oriented(path)?;
        let layout = layout_for(&decoded, options);

        // printpdf wants the image re-encoded from bytes rather than from an
        // in-memory buffer, and going through PNG keeps every source format on
        // one code path without a second lossy generation.
        let mut encoded = std::io::Cursor::new(Vec::new());
        decoded
            .write_to(&mut encoded, image::ImageFormat::Png)
            .map_err(|_| PdfError::UnsupportedImage(path.clone()))?;

        let raw = RawImage::decode_from_bytes(&encoded.into_inner(), &mut Vec::new())
            .map_err(|_| PdfError::UnsupportedImage(path.clone()))?;

        let id = document.add_image(&raw);

        // XObjectTransform scales are relative to the image's natural size at
        // the given dpi, so compute the dpi that makes it land at the size the
        // layout asked for.
        let horizontal_dpi = decoded.width() as f32 / (layout.width / 72.0);

        let operations = vec![Op::UseXobject {
            id,
            transform: XObjectTransform {
                translate_x: Some(Pt(layout.x)),
                translate_y: Some(Pt(layout.y)),
                scale_x: Some(1.0),
                scale_y: Some(
                    layout.height / layout.width * decoded.width() as f32 / decoded.height() as f32,
                ),
                dpi: Some(horizontal_dpi),
                ..Default::default()
            },
        }];

        pages.push(PdfPage::new(
            Mm(points_to_mm(layout.page_width)),
            Mm(points_to_mm(layout.page_height)),
            operations,
        ));
    }

    if let Some(tick) = progress.as_mut() {
        tick(Progress::new(total, Some(total), "writing output"));
    }

    let bytes = document
        .with_pages(pages)
        .save(&PdfSaveOptions::default(), &mut Vec::new());

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| PdfError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    std::fs::write(output, bytes).map_err(|source| PdfError::Io {
        path: output.to_path_buf(),
        source,
    })
}

/// Read an image and apply its EXIF orientation.
///
/// A photo straight off a phone is usually stored in the sensor's orientation
/// with a tag saying which way up it belongs; ignoring the tag puts every
/// portrait photo on its side.
fn load_oriented(path: &Path) -> Result<DynamicImage> {
    let reader = ImageReader::open(path)
        .map_err(|source| PdfError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .with_guessed_format()
        .map_err(|source| PdfError::Io {
            path: path.to_path_buf(),
            source,
        })?;

    let mut decoder = reader
        .into_decoder()
        .map_err(|_| PdfError::UnsupportedImage(path.to_path_buf()))?;

    let orientation = image::ImageDecoder::orientation(&mut decoder)
        .unwrap_or(image::metadata::Orientation::NoTransforms);

    let mut decoded = DynamicImage::from_decoder(decoder)
        .map_err(|_| PdfError::UnsupportedImage(path.to_path_buf()))?;
    decoded.apply_orientation(orientation);

    Ok(decoded)
}

fn layout_for(image: &DynamicImage, options: ImagesToPdfOptions) -> Layout {
    let image_width = image.width().max(1) as f32;
    let image_height = image.height().max(1) as f32;

    let Some((page_width, page_height)) = options.page_size.points() else {
        // One page exactly the size of the image.
        let width = image_width / options.dpi * 72.0;
        let height = image_height / options.dpi * 72.0;

        return Layout {
            page_width: width,
            page_height: height,
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
    };

    let margin = options.margin.max(0.0);
    // A margin wider than the page would invert the box; clamp instead of
    // producing a negative-sized image.
    let box_width = (page_width - margin * 2.0).max(1.0);
    let box_height = (page_height - margin * 2.0).max(1.0);

    let (width, height) = match options.fit {
        Fit::Stretch => (box_width, box_height),
        Fit::Contain | Fit::Cover => {
            let horizontal = box_width / image_width;
            let vertical = box_height / image_height;

            let scale = match options.fit {
                Fit::Contain => horizontal.min(vertical),
                _ => horizontal.max(vertical),
            };

            (image_width * scale, image_height * scale)
        }
    };

    Layout {
        page_width,
        page_height,
        // Centred, which is what both Contain's margins and Cover's crop want.
        x: (page_width - width) / 2.0,
        y: (page_height - height) / 2.0,
        width,
        height,
    }
}

fn points_to_mm(points: f32) -> f32 {
    points * 25.4 / 72.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(image::RgbImage::new(width, height))
    }

    #[test]
    fn known_page_sizes_are_in_points() {
        assert_eq!(PageSize::Letter.points(), Some((612.0, 792.0)));
        assert_eq!(PageSize::FitImage.points(), None);
        assert_eq!(
            PageSize::Custom {
                width: 100.0,
                height: 200.0
            }
            .points(),
            Some((100.0, 200.0))
        );
    }

    #[test]
    fn contain_fits_the_whole_image_on_the_page() {
        // A wide image on a portrait page is limited by width.
        let layout = layout_for(
            &image(2000, 1000),
            ImagesToPdfOptions {
                page_size: PageSize::A4,
                fit: Fit::Contain,
                margin: 0.0,
                dpi: 300.0,
            },
        );

        assert!(layout.width <= layout.page_width + 0.01);
        assert!(layout.height <= layout.page_height + 0.01);
        assert!(
            (layout.width / layout.height - 2.0).abs() < 0.01,
            "aspect ratio should be preserved"
        );
    }

    #[test]
    fn cover_fills_the_page_and_overflows() {
        let layout = layout_for(
            &image(2000, 1000),
            ImagesToPdfOptions {
                page_size: PageSize::A4,
                fit: Fit::Cover,
                ..Default::default()
            },
        );

        assert!(layout.width >= layout.page_width - 0.01);
        assert!(layout.height >= layout.page_height - 0.01);
        assert!((layout.width / layout.height - 2.0).abs() < 0.01);
    }

    #[test]
    fn stretch_matches_the_page_exactly() {
        let layout = layout_for(
            &image(2000, 1000),
            ImagesToPdfOptions {
                page_size: PageSize::Letter,
                fit: Fit::Stretch,
                ..Default::default()
            },
        );

        assert!((layout.width - 612.0).abs() < 0.01);
        assert!((layout.height - 792.0).abs() < 0.01);
    }

    #[test]
    fn a_margin_shrinks_the_available_box() {
        let without = layout_for(&image(1000, 1000), ImagesToPdfOptions::default());
        let with = layout_for(
            &image(1000, 1000),
            ImagesToPdfOptions {
                margin: 36.0,
                ..Default::default()
            },
        );

        assert!(with.width < without.width);
        assert!((with.x - 36.0).abs() < 1.0, "should sit inside the margin");
    }

    #[test]
    fn an_absurd_margin_does_not_invert_the_page() {
        let layout = layout_for(
            &image(1000, 1000),
            ImagesToPdfOptions {
                margin: 10_000.0,
                ..Default::default()
            },
        );

        assert!(layout.width > 0.0 && layout.height > 0.0);
    }

    #[test]
    fn fit_image_sizes_the_page_to_the_image() {
        // 300 pixels at 300 dpi is one inch, which is 72 points.
        let layout = layout_for(
            &image(300, 600),
            ImagesToPdfOptions {
                page_size: PageSize::FitImage,
                dpi: 300.0,
                ..Default::default()
            },
        );

        assert!((layout.page_width - 72.0).abs() < 0.01);
        assert!((layout.page_height - 144.0).abs() < 0.01);
        assert_eq!(layout.x, 0.0);
        assert_eq!(layout.y, 0.0);
    }

    #[test]
    fn the_image_is_centred_on_the_page() {
        let layout = layout_for(
            &image(1000, 500),
            ImagesToPdfOptions {
                page_size: PageSize::A4,
                fit: Fit::Contain,
                ..Default::default()
            },
        );

        let left = layout.x;
        let right = layout.page_width - (layout.x + layout.width);
        assert!((left - right).abs() < 0.01, "left {left} right {right}");
    }

    #[test]
    fn a_zero_sized_image_does_not_divide_by_zero() {
        let layout = layout_for(&image(0, 0), ImagesToPdfOptions::default());
        assert!(layout.width.is_finite() && layout.width > 0.0);
    }

    #[test]
    fn points_convert_to_millimetres() {
        assert!((points_to_mm(72.0) - 25.4).abs() < 0.001);
    }
}
