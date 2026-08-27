use lopdf::{Object, ObjectId};

use crate::assemble::materialize_inherited_attributes;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::pages::PageRange;

/// Rotate the selected pages by `degrees`, which must be a multiple of 90.
///
/// Rotation is relative to each page's current `/Rotate` value and normalised
/// into the `0..360` range the specification requires. Selecting the same page
/// twice rotates it once: repeats in a range mark pages rather than compounding.
pub fn rotate(doc: &mut Document, pages: &PageRange, degrees: i32) -> Result<()> {
    if degrees % 90 != 0 {
        return Err(PdfError::InvalidRotation(degrees));
    }

    let page_count = doc.page_count();
    let selected = pages.resolve_unique(page_count)?;

    if selected.is_empty() {
        return Err(PdfError::EmptySelection);
    }

    // A page can inherit /Rotate from its parent. Reading the inherited value
    // and writing the result onto the page itself keeps the rotation of
    // unselected siblings unchanged.
    materialize_inherited_attributes(&mut doc.inner)?;

    let page_ids: Vec<ObjectId> = doc.inner.get_pages().into_values().collect();

    for position in selected {
        let page_id = page_ids[position];
        let current = doc
            .inner
            .get_dictionary(page_id)
            .ok()
            .and_then(|dict| dict.get(b"Rotate").ok())
            .and_then(|object| object.as_i64().ok())
            .unwrap_or(0);

        let rotation = normalise(current as i32 + degrees);

        let page = doc.inner.get_object_mut(page_id)?.as_dict_mut()?;
        if rotation == 0 {
            page.remove(b"Rotate");
        } else {
            page.set("Rotate", Object::Integer(rotation as i64));
        }
    }

    Ok(())
}

/// Fold an arbitrary multiple of 90 into `0..360`.
fn normalise(degrees: i32) -> i32 {
    degrees.rem_euclid(360)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_fold_into_a_single_turn() {
        assert_eq!(normalise(0), 0);
        assert_eq!(normalise(90), 90);
        assert_eq!(normalise(360), 0);
        assert_eq!(normalise(450), 90);
        assert_eq!(normalise(-90), 270);
        assert_eq!(normalise(-450), 270);
    }
}
