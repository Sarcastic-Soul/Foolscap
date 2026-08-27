//! Conversion to and from other formats.
//!
//! Available with the `convert` feature. Office conversion additionally needs
//! LibreOffice installed at run time; [`office::is_available`] reports whether
//! it is, so callers can say so rather than failing at the last moment.

pub mod images;
pub mod office;

#[cfg(feature = "render")]
mod raster;

pub use images::{images_to_pdf, images_to_pdf_with_progress, Fit, ImagesToPdfOptions, PageSize};
pub use office::{office_to_pdf, pdf_to_office, OfficeFormat};

#[cfg(feature = "render")]
pub use raster::{
    pdf_to_images, pdf_to_images_with_progress, plan, ImageFormat, PdfToImagesOptions,
};
