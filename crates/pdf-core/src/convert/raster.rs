//! PDF to images. Needs both `convert` and `render`.

use std::path::{Path, PathBuf};

use crate::error::{PdfError, Result};
use crate::pages::PageRange;
use crate::progress::{Progress, ProgressFn};
use crate::render::{PageRenderer, Scale};

/// Raster formats pages can be written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    /// Lossless, larger. The right default for pages of text and line art.
    #[default]
    Png,
    /// Lossy, smaller. Better for photographic pages.
    Jpeg,
}

impl ImageFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }
}

/// Options for [`pdf_to_images`].
#[derive(Debug, Clone, PartialEq)]
pub struct PdfToImagesOptions {
    pub pages: PageRange,
    pub dpi: f32,
    pub format: ImageFormat,
    /// JPEG quality, ignored for PNG.
    pub quality: u8,
}

impl Default for PdfToImagesOptions {
    fn default() -> Self {
        Self {
            pages: PageRange::All,
            dpi: 150.0,
            format: ImageFormat::Png,
            quality: 85,
        }
    }
}

/// The paths [`pdf_to_images`] would write, without writing anything.
///
/// The CLI needs these in advance to ask before overwriting.
pub fn plan(input: &Path, out_dir: &Path, options: &PdfToImagesOptions) -> Result<Vec<PathBuf>> {
    let renderer = PageRenderer::open(input)?;
    let selected = options.pages.resolve(renderer.page_count())?;
    Ok(output_paths(
        input,
        out_dir,
        &selected,
        renderer.page_count(),
        options,
    ))
}

/// Rasterise the selected pages into `out_dir`.
pub fn pdf_to_images(
    input: &Path,
    out_dir: &Path,
    options: &PdfToImagesOptions,
) -> Result<Vec<PathBuf>> {
    pdf_to_images_with_progress(input, out_dir, options, None)
}

/// [`pdf_to_images`], reporting one progress tick per page.
pub fn pdf_to_images_with_progress(
    input: &Path,
    out_dir: &Path,
    options: &PdfToImagesOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<Vec<PathBuf>> {
    if !(options.dpi.is_finite() && options.dpi > 0.0) {
        return Err(PdfError::Render {
            path: input.to_path_buf(),
            reason: format!("{} is not a usable resolution", options.dpi),
        });
    }

    let renderer = PageRenderer::open(input)?;
    let selected = options.pages.resolve(renderer.page_count())?;

    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    let paths = output_paths(input, out_dir, &selected, renderer.page_count(), options);
    let total = selected.len();

    std::fs::create_dir_all(out_dir).map_err(|source| PdfError::Io {
        path: out_dir.to_path_buf(),
        source,
    })?;

    for (index, (page, path)) in selected.iter().zip(&paths).enumerate() {
        if let Some(tick) = progress.as_mut() {
            tick(Progress::new(
                index,
                Some(total),
                format!("writing {}", path.display()),
            ));
        }

        let rendered = renderer.render(*page, Scale::Dpi(options.dpi))?;
        write_image(&rendered, path, options)?;
    }

    Ok(paths)
}

fn output_paths(
    input: &Path,
    out_dir: &Path,
    selected: &[usize],
    page_count: usize,
    options: &PdfToImagesOptions,
) -> Vec<PathBuf> {
    let stem = input
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page".to_string());
    let width = page_count.max(1).to_string().len();
    let extension = options.format.extension();

    selected
        .iter()
        .map(|page| {
            out_dir.join(format!(
                "{stem}-{:0width$}.{extension}",
                page + 1,
                width = width
            ))
        })
        .collect()
}

fn write_image(
    page: &crate::render::RenderedPage,
    path: &Path,
    options: &PdfToImagesOptions,
) -> Result<()> {
    use image::{ExtendedColorType, ImageEncoder};

    let file = std::fs::File::create(path).map_err(|source| PdfError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let writer = std::io::BufWriter::new(file);

    let failed = |reason: String| PdfError::Render {
        path: path.to_path_buf(),
        reason,
    };

    match options.format {
        ImageFormat::Png => {
            let colour = match page.channels {
                4 => ExtendedColorType::Rgba8,
                3 => ExtendedColorType::Rgb8,
                1 => ExtendedColorType::L8,
                other => return Err(failed(format!("{other} channels per pixel"))),
            };

            image::codecs::png::PngEncoder::new(writer)
                .write_image(&page.pixels, page.width, page.height, colour)
                .map_err(|source| failed(source.to_string()))
        }
        ImageFormat::Jpeg => {
            // JPEG has no alpha channel, so the RGBA the renderer produces has
            // to be flattened first.
            let rgb = flatten_to_rgb(page);

            image::codecs::jpeg::JpegEncoder::new_with_quality(writer, options.quality)
                .write_image(&rgb, page.width, page.height, ExtendedColorType::Rgb8)
                .map_err(|source| failed(source.to_string()))
        }
    }
}

/// Drop the alpha channel, compositing over white.
///
/// White rather than black: a PDF page is paper, and a transparent region
/// should read as unprinted rather than as ink.
fn flatten_to_rgb(page: &crate::render::RenderedPage) -> Vec<u8> {
    if page.channels == 3 {
        return page.pixels.clone();
    }

    let mut rgb = Vec::with_capacity(page.width as usize * page.height as usize * 3);

    for pixel in page.pixels.chunks_exact(page.channels as usize) {
        match page.channels {
            4 => {
                let alpha = pixel[3] as u32;
                for channel in &pixel[..3] {
                    let over_white = (*channel as u32 * alpha + 255 * (255 - alpha)) / 255;
                    rgb.push(over_white as u8);
                }
            }
            1 => rgb.extend_from_slice(&[pixel[0], pixel[0], pixel[0]]),
            _ => rgb.extend_from_slice(&pixel[..3.min(pixel.len())]),
        }
    }

    rgb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::RenderedPage;

    #[test]
    fn formats_have_the_expected_extensions() {
        assert_eq!(ImageFormat::Png.extension(), "png");
        assert_eq!(ImageFormat::Jpeg.extension(), "jpg");
    }

    #[test]
    fn names_are_padded_to_the_page_count() {
        let options = PdfToImagesOptions::default();
        let paths = output_paths(
            Path::new("/in/report.pdf"),
            Path::new("/out"),
            &[0, 9],
            120,
            &options,
        );

        assert_eq!(paths[0], Path::new("/out/report-001.png"));
        assert_eq!(paths[1], Path::new("/out/report-010.png"));
    }

    #[test]
    fn the_extension_follows_the_format() {
        let options = PdfToImagesOptions {
            format: ImageFormat::Jpeg,
            ..Default::default()
        };
        let paths = output_paths(
            Path::new("/in/report.pdf"),
            Path::new("/out"),
            &[0],
            1,
            &options,
        );

        assert_eq!(paths[0], Path::new("/out/report-1.jpg"));
    }

    #[test]
    fn opaque_pixels_survive_flattening() {
        let page = RenderedPage {
            width: 2,
            height: 1,
            channels: 4,
            pixels: vec![10, 20, 30, 255, 40, 50, 60, 255],
        };

        assert_eq!(flatten_to_rgb(&page), vec![10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn transparent_pixels_become_white() {
        let page = RenderedPage {
            width: 1,
            height: 1,
            channels: 4,
            pixels: vec![0, 0, 0, 0],
        };

        assert_eq!(flatten_to_rgb(&page), vec![255, 255, 255]);
    }

    #[test]
    fn grey_pixels_expand_to_three_channels() {
        let page = RenderedPage {
            width: 2,
            height: 1,
            channels: 1,
            pixels: vec![128, 64],
        };

        assert_eq!(flatten_to_rgb(&page), vec![128, 128, 128, 64, 64, 64]);
    }
}
