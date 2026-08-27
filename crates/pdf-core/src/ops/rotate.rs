use crate::document::Document;
use crate::error::Result;
use crate::pages::PageRange;

/// Rotate the selected pages by `degrees`, which must be a multiple of 90.
///
/// Rotation is relative to each page's current `/Rotate` value.
pub fn rotate(_doc: &mut Document, _pages: &PageRange, _degrees: i32) -> Result<()> {
    todo!("stage 1: rotate")
}
