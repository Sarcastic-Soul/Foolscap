//! Conversion to and from other formats.
//!
//! Available with the `convert` feature. Office conversion additionally needs
//! LibreOffice installed at run time; [`office::is_available`] reports whether
//! it is, so callers can say so rather than failing at the last moment.

pub mod html;
pub mod images;
pub mod markdown;
pub mod office;
pub mod text;

#[cfg(feature = "render")]
mod raster;

#[cfg(feature = "render")]
pub mod archive;

pub use html::{escape_html, html_to_pdf, is_available as html_is_available};
pub use images::{images_to_pdf, images_to_pdf_with_progress, Fit, ImagesToPdfOptions, PageSize};
pub use markdown::{
    markdown_file_to_pdf, markdown_file_to_pdf_with_progress, markdown_to_pdf,
    markdown_to_pdf_with_progress,
};
pub use office::{office_to_pdf, pdf_to_office, OfficeFormat};
pub use text::{
    text_file_to_pdf, text_file_to_pdf_with_progress, text_to_pdf, text_to_pdf_with_progress,
    TextToPdfOptions, DEFAULT_FONT_SIZE, DEFAULT_MARGIN,
};

#[cfg(feature = "render")]
pub use archive::{pdf_to_zip, pdf_to_zip_with_progress, zip_plan};

#[cfg(feature = "render")]
pub use html::{pdf_to_html, pdf_to_html_with_progress, PdfToHtmlOptions};

#[cfg(feature = "render")]
pub use raster::{
    pdf_to_images, pdf_to_images_with_progress, plan, ImageFormat, PdfToImagesOptions,
};
