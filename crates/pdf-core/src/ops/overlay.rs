//! Overlay one PDF on top of another: each page of `overlay` is drawn over the
//! corresponding page of `base`, preserving the base page's content underneath.
//!
//! Available only with the `render` feature, because the overlay page is
//! rasterised and stamped as a full-page image. That trades vector fidelity
//! for robustness: resource-name collisions between the two documents cannot
//! corrupt either page.

use std::io::Cursor;

use image::{DynamicImage, ImageBuffer, Rgb};

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::ops::stamp::{stamp, StampContent, StampLayer, StampOptions};
use crate::pages::PageRange;
use crate::render::{PageRenderer, Scale};

/// Overlay `overlay_path` onto `base_path`, writing the result to `out_path`.
#[cfg(feature = "render")]
pub fn overlay(
    base_path: &std::path::Path,
    overlay_path: &std::path::Path,
    out_path: &std::path::Path,
    password: Option<&str>,
) -> Result<()> {
    let mut doc = Document::open_with(base_path, password)?;
    let renderer = PageRenderer::open_with(overlay_path, password)?;
    let base_count = doc.page_count();
    let overlay_count = renderer.page_count();
    if overlay_count == 0 {
        return Err(PdfError::EmptySelection);
    }

    for i in 0..base_count {
        let ov = i.min(overlay_count - 1);
        let rendered = renderer.render(ov, Scale::Dpi(72.0))?;
        let img = DynamicImage::ImageRgb8(
            ImageBuffer::<Rgb<u8>, _>::from_raw(
                rendered.width,
                rendered.height,
                rendered.pixels.clone(),
            )
            .ok_or_else(|| PdfError::Internal("bad render buffer".into()))?,
        );
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .map_err(|e| PdfError::Io {
                path: out_path.to_path_buf(),
                source: std::io::Error::other(e.to_string()),
            })?;

        let opts = StampOptions {
            content: StampContent::Image(buf),
            anchor: crate::ops::stamp::Anchor::BottomLeft,
            margin: 0.0,
            opacity: 1.0,
            image_scale: 1.0,
            layer: StampLayer::Over,
            ..Default::default()
        };
        let range = PageRange::parse(&format!("{}", i + 1))?;
        stamp(&mut doc, &range, &opts)?;
    }

    doc.save(out_path)?;
    Ok(())
}
