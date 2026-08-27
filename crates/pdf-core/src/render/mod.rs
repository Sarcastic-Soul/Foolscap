//! Page rendering, backed by MuPDF.
//!
//! Available only with the `render` feature, which pulls in `mupdf-rs` and its
//! vendored C sources.
//!
//! Renderers hand back pixel buffers, not files. The CLI encodes them as PNG;
//! the GTK front end will hand them straight to a texture. Nothing in here
//! knows what an image file is.

mod cache;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mupdf::{Colorspace, Matrix};

use crate::error::{PdfError, Result};

pub use cache::CacheStats;
use cache::RenderCache;

/// PDF user space is defined as 72 units to the inch, so this is the scale
/// factor of one.
pub const POINTS_PER_INCH: f32 = 72.0;

/// The default resolution for `render` when the caller does not say.
pub const DEFAULT_DPI: f32 = 150.0;

/// A rendered page, as 8-bit samples in row-major order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    /// Samples per pixel: 4 for RGBA, which is what this module always
    /// produces. Carried explicitly so callers do not have to assume.
    pub channels: u8,
    pub pixels: Vec<u8>,
}

impl RenderedPage {
    /// Bytes per row, for callers that need a stride.
    pub fn stride(&self) -> usize {
        self.width as usize * self.channels as usize
    }

    /// True when the buffer length does not match the stated geometry.
    fn is_consistent(&self) -> bool {
        self.pixels.len() == self.stride() * self.height as usize
    }
}

/// How large a rendered page should be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// A resolution in dots per inch.
    Dpi(f32),
    /// Fit inside a square of this many pixels, preserving aspect ratio. This
    /// is what thumbnails want: a predictable maximum, whatever the page size.
    FitBox(u32),
}

impl Scale {
    /// The zoom factor to apply to a page of the given size in points.
    fn factor(&self, page_width: f32, page_height: f32) -> f32 {
        match *self {
            Scale::Dpi(dpi) => dpi / POINTS_PER_INCH,
            Scale::FitBox(edge) => {
                let longest = page_width.max(page_height);
                if longest <= 0.0 {
                    return 1.0;
                }
                edge as f32 / longest
            }
        }
    }

    /// The cache key for this scale. Floating point is quantised to a
    /// thousandth so that two requests for the same resolution hit the same
    /// entry despite arithmetic noise.
    fn key(&self) -> (u8, i64) {
        match *self {
            Scale::Dpi(dpi) => (0, (dpi * 1000.0).round() as i64),
            Scale::FitBox(edge) => (1, edge as i64),
        }
    }
}

/// Renders pages of one document, caching what it has already drawn.
///
/// MuPDF's context is thread-local, so a renderer belongs to the thread that
/// created it. The GUI will own one per worker thread rather than sharing.
pub struct PageRenderer {
    inner: mupdf::Document,
    path: PathBuf,
    page_count: usize,
    cache: RefCell<RenderCache>,
}

impl PageRenderer {
    /// Default number of rendered pages to keep. Enough for a screenful of
    /// thumbnails plus the page being read.
    pub const DEFAULT_CACHE_CAPACITY: usize = 32;

    /// Open a document for rendering.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_capacity(path, Self::DEFAULT_CACHE_CAPACITY)
    }

    /// Open a document, choosing how many rendered pages to retain.
    pub fn open_with_capacity(path: impl AsRef<Path>, capacity: usize) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(PdfError::NotFound(path.to_path_buf()));
        }

        let inner = mupdf::Document::open(path).map_err(|source| render_error(path, source))?;

        if inner
            .needs_password()
            .map_err(|source| render_error(path, source))?
        {
            return Err(PdfError::Encrypted(path.to_path_buf()));
        }

        let page_count = inner
            .page_count()
            .map_err(|source| render_error(path, source))?
            .max(0) as usize;

        Ok(Self {
            inner,
            path: path.to_path_buf(),
            page_count,
            cache: RefCell::new(RenderCache::new(capacity)),
        })
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Render one zero-indexed page at the given scale.
    ///
    /// Repeated requests for the same page and scale come from the cache.
    pub fn render(&self, page: usize, scale: Scale) -> Result<Arc<RenderedPage>> {
        if page >= self.page_count {
            return Err(PdfError::PageOutOfRange {
                requested: page + 1,
                total: self.page_count,
            });
        }

        let key = (page, scale.key());
        if let Some(hit) = self.cache.borrow_mut().get(&key) {
            tracing::trace!(page, "render cache hit");
            return Ok(hit);
        }

        let rendered = Arc::new(self.draw(page, scale)?);
        self.cache.borrow_mut().insert(key, Arc::clone(&rendered));
        Ok(rendered)
    }

    /// Render a page scaled to fit inside a square of `max_edge` pixels.
    pub fn thumbnail(&self, page: usize, max_edge: u32) -> Result<Arc<RenderedPage>> {
        self.render(page, Scale::FitBox(max_edge))
    }

    /// The page's size in points, before any scaling.
    pub fn page_size(&self, page: usize) -> Result<(f32, f32)> {
        if page >= self.page_count {
            return Err(PdfError::PageOutOfRange {
                requested: page + 1,
                total: self.page_count,
            });
        }

        let loaded = self
            .inner
            .load_page(page as i32)
            .map_err(|source| render_error(&self.path, source))?;
        let bounds = loaded
            .bounds()
            .map_err(|source| render_error(&self.path, source))?;

        Ok((bounds.x1 - bounds.x0, bounds.y1 - bounds.y0))
    }

    /// The text already embedded in a page, in reading order.
    ///
    /// This is the text a PDF carries, not the result of recognising anything:
    /// a page of scanned paper returns nothing at all. That distinction is
    /// what lets OCR skip the pages that do not need it.
    pub fn page_text(&self, page: usize) -> Result<String> {
        if page >= self.page_count {
            return Err(PdfError::PageOutOfRange {
                requested: page + 1,
                total: self.page_count,
            });
        }

        let loaded = self
            .inner
            .load_page(page as i32)
            .map_err(|source| render_error(&self.path, source))?;

        let text_page = loaded
            .to_text_page(mupdf::TextPageFlags::empty())
            .map_err(|source| render_error(&self.path, source))?;

        text_page
            .to_text()
            .map_err(|source| render_error(&self.path, source))
    }

    /// Whether a page already carries selectable text.
    ///
    /// Whitespace does not count: a page whose only text is a stray space is
    /// still a scan as far as anyone reading it is concerned.
    pub fn page_has_text(&self, page: usize) -> Result<bool> {
        Ok(!self.page_text(page)?.trim().is_empty())
    }

    /// How the cache is doing. Useful for deciding whether the GUI needs a
    /// bigger one.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.borrow().stats()
    }

    /// Drop everything cached, for when the document has changed underneath.
    pub fn clear_cache(&self) {
        self.cache.borrow_mut().clear();
    }

    fn draw(&self, page: usize, scale: Scale) -> Result<RenderedPage> {
        let loaded = self
            .inner
            .load_page(page as i32)
            .map_err(|source| render_error(&self.path, source))?;

        let bounds = loaded
            .bounds()
            .map_err(|source| render_error(&self.path, source))?;
        let factor = scale.factor(bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);

        if !factor.is_finite() || factor <= 0.0 {
            return Err(PdfError::Render {
                path: self.path.clone(),
                reason: format!("scale {scale:?} produced a zoom factor of {factor}"),
            });
        }

        let matrix = Matrix::new_scale(factor, factor);
        // Alpha on, so the buffer is RGBA and needs no widening for a GPU
        // texture. show_extras draws annotations and widgets, which is what a
        // viewer should show.
        let pixmap = loaded
            .to_pixmap(&matrix, &Colorspace::device_rgb(), true, true)
            .map_err(|source| render_error(&self.path, source))?;

        let rendered = RenderedPage {
            width: pixmap.width(),
            height: pixmap.height(),
            channels: pixmap.n(),
            pixels: pixmap.samples().to_vec(),
        };

        if !rendered.is_consistent() {
            return Err(PdfError::Render {
                path: self.path.clone(),
                reason: format!(
                    "MuPDF returned {} bytes for a {}x{} image with {} channels",
                    rendered.pixels.len(),
                    rendered.width,
                    rendered.height,
                    rendered.channels
                ),
            });
        }

        tracing::debug!(
            page,
            width = rendered.width,
            height = rendered.height,
            "rendered page"
        );

        Ok(rendered)
    }
}

impl std::fmt::Debug for PageRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageRenderer")
            .field("path", &self.path)
            .field("pages", &self.page_count)
            .field("cache", &self.cache.borrow().stats())
            .finish()
    }
}

fn render_error(path: &Path, source: mupdf::Error) -> PdfError {
    PdfError::Render {
        path: path.to_path_buf(),
        reason: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_scales_relative_to_seventy_two_points() {
        assert_eq!(Scale::Dpi(72.0).factor(595.0, 842.0), 1.0);
        assert_eq!(Scale::Dpi(144.0).factor(595.0, 842.0), 2.0);
        assert_eq!(Scale::Dpi(36.0).factor(595.0, 842.0), 0.5);
    }

    #[test]
    fn fit_box_uses_the_longest_edge() {
        // A4 portrait: the height is what has to fit.
        let factor = Scale::FitBox(842).factor(595.0, 842.0);
        assert!((factor - 1.0).abs() < f32::EPSILON);

        let landscape = Scale::FitBox(595).factor(842.0, 595.0);
        assert!((landscape - 595.0 / 842.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_degenerate_page_does_not_divide_by_zero() {
        assert_eq!(Scale::FitBox(256).factor(0.0, 0.0), 1.0);
    }

    #[test]
    fn cache_keys_distinguish_scale_kinds() {
        assert_ne!(Scale::Dpi(150.0).key(), Scale::FitBox(150).key());
        assert_eq!(Scale::Dpi(150.0).key(), Scale::Dpi(150.0).key());
        assert_ne!(Scale::Dpi(150.0).key(), Scale::Dpi(150.001).key());
    }

    #[test]
    fn stride_accounts_for_channels() {
        let page = RenderedPage {
            width: 3,
            height: 2,
            channels: 4,
            pixels: vec![0; 24],
        };
        assert_eq!(page.stride(), 12);
        assert!(page.is_consistent());
    }
}
