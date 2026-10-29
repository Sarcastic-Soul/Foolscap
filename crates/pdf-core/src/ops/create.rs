//! Create new blank documents.

use lopdf::content::Content;
use lopdf::{dictionary, Object, Stream};

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::ops::crop::PageSize;

/// Build a document of `pages` empty pages of the given size.
///
/// Every page carries its own `/MediaBox` at the origin and an empty content
/// stream, and the document is wired the way the other producers in this
/// crate are: a `/Pages` parent, a catalog pointing at it, and a trailer with
/// a `/Root`. A `pages` of zero is refused — a PDF with no pages is not a
/// document any reader will open. [`PageSize::FitImage`] names no size and is
/// refused too.
pub fn create_blank(pages: usize, size: PageSize) -> Result<Document> {
    if pages == 0 {
        return Err(PdfError::EmptySelection);
    }
    let (width, height) = size.points().ok_or_else(|| PdfError::InvalidMediaBox {
        reason: "FitImage has no fixed size; pick a4, letter, legal, or WxH".into(),
    })?;

    let mut doc = lopdf::Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    // An empty content stream per page: no operations, but a real object, so
    // the page is well-formed rather than merely missing its /Contents.
    let empty = Content { operations: vec![] }.encode().map_err(|source| {
        PdfError::Internal(format!(
            "could not encode an empty content stream: {source}"
        ))
    })?;

    let mut kids = Vec::with_capacity(pages);
    for _ in 0..pages {
        let content_id = doc.add_object(Stream::new(dictionary! {}, empty.clone()));
        kids.push(Object::Reference(doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(width),
                Object::Real(height),
            ],
            "Contents" => content_id,
        })));
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => pages as i64,
            "Kids" => Object::Array(kids),
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    Ok(Document::from_lopdf(doc, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_pages_carry_the_requested_size() {
        let doc = create_blank(3, PageSize::A4).unwrap();
        assert_eq!(doc.page_count(), 3);

        let pages = doc.inner.get_pages();
        assert_eq!(pages.len(), 3);
        for page_id in pages.into_values() {
            let page = doc.inner.get_dictionary(page_id).unwrap();
            let media = page.get(b"MediaBox").unwrap().as_array().unwrap();
            let rounded = |entry: &Object| match entry {
                Object::Integer(value) => *value,
                Object::Real(value) => value.round() as i64,
                other => panic!("a size entry that is not a number: {other:?}"),
            };
            assert_eq!(rounded(&media[2]), 595);
            assert_eq!(rounded(&media[3]), 842);
            assert!(page.get(b"Contents").is_ok());
        }
    }

    #[test]
    fn zero_pages_and_fit_image_are_refused() {
        assert!(matches!(
            create_blank(0, PageSize::A4).unwrap_err(),
            PdfError::EmptySelection
        ));
        assert!(matches!(
            create_blank(1, PageSize::FitImage).unwrap_err(),
            PdfError::InvalidMediaBox { .. }
        ));
    }
}
