//! Grafting an invisible text layer onto an existing page.
//!
//! Tesseract can produce a PDF containing nothing but positioned, invisible
//! text. Laying that over the original page — rather than keeping Tesseract's
//! own rendering of it — is what makes a document searchable without
//! reprinting it at whatever resolution the recogniser happened to see.

use std::collections::HashMap;

use lopdf::content::Content;
use lopdf::{Dictionary, Object, ObjectId, Stream};

use crate::error::{PdfError, Result};

/// Prefix for resource names brought in from the text layer.
///
/// Imported fonts share a namespace with the page's own, and `/F1` is the most
/// popular font name in existence. A prefix nobody else uses avoids silently
/// repointing the page's real text at a recogniser's font.
const IMPORT_PREFIX: &str = "FsOcr";

/// Imported fonts, keyed by their name in the source: the name to use in the
/// target, and the object they were copied to.
type FontImports = HashMap<Vec<u8>, (Vec<u8>, ObjectId)>;

/// Copy the invisible text from one page of `source` onto a page of `target`.
///
/// `scale` converts the source page's coordinates into the target's, for when
/// the recogniser worked at a different size than the page.
pub(crate) fn graft_text_layer(
    target: &mut lopdf::Document,
    source: &lopdf::Document,
    target_page: ObjectId,
    scale: f32,
) -> Result<()> {
    let Some(source_page) = source.get_pages().into_values().next() else {
        return Err(PdfError::Internal(
            "the recognised text layer has no pages".into(),
        ));
    };

    let content = source
        .get_page_content(source_page)
        .map_err(|source| PdfError::Internal(format!("could not read the text layer: {source}")))?;
    let decoded = Content::decode(&content).map_err(|source| {
        PdfError::Internal(format!("could not decode the text layer: {source}"))
    })?;

    // Nothing recognised: leave the page exactly as it was rather than
    // appending an empty content stream.
    if !draws_any_text(&decoded) {
        return Ok(());
    }

    let fonts = import_fonts(target, source, source_page)?;
    let operations = rewrite_font_names(decoded, &fonts);

    let mut wrapped = Vec::with_capacity(operations.len() + 4);
    // Isolate the graft: the page's own graphics state must not leak into the
    // text, and the text's must not leak back out.
    wrapped.push(lopdf::content::Operation::new("q", vec![]));
    if (scale - 1.0).abs() > f32::EPSILON {
        wrapped.push(lopdf::content::Operation::new(
            "cm",
            vec![
                scale.into(),
                0.into(),
                0.into(),
                scale.into(),
                0.into(),
                0.into(),
            ],
        ));
    }
    wrapped.extend(operations);
    wrapped.push(lopdf::content::Operation::new("Q", vec![]));

    let encoded = Content {
        operations: wrapped,
    }
    .encode()
    .map_err(|source| PdfError::Internal(format!("could not encode the text layer: {source}")))?;

    let layer_id = target.add_object(Stream::new(Dictionary::new(), encoded));

    append_content(target, target_page, layer_id)?;
    merge_fonts(target, target_page, &fonts)?;

    Ok(())
}

/// Whether a content stream actually draws any glyphs.
fn draws_any_text(content: &Content) -> bool {
    content
        .operations
        .iter()
        .any(|operation| matches!(operation.operator.as_str(), "Tj" | "TJ" | "'" | "\""))
}

/// Copy the text layer's fonts into the target, returning old name to new name.
fn import_fonts(
    target: &mut lopdf::Document,
    source: &lopdf::Document,
    source_page: ObjectId,
) -> Result<FontImports> {
    let mut mapping = HashMap::new();

    let Some(resources) = page_resources(source, source_page) else {
        return Ok(mapping);
    };
    let Some(fonts) = resources
        .get(b"Font")
        .ok()
        .and_then(|object| resolve_dictionary(source, object))
    else {
        return Ok(mapping);
    };

    let mut copied: HashMap<ObjectId, ObjectId> = HashMap::new();

    for (index, (name, value)) in fonts.iter().enumerate() {
        let new_id = deep_copy(target, source, value, &mut copied, 0)?;
        let new_name = format!("{IMPORT_PREFIX}{index}").into_bytes();
        mapping.insert(name.clone(), (new_name, new_id));
    }

    Ok(mapping)
}

/// Copy an object and everything it references into `target`.
///
/// `copied` keeps shared objects shared: a font descriptor referenced by three
/// fonts is copied once, not three times.
fn deep_copy(
    target: &mut lopdf::Document,
    source: &lopdf::Document,
    object: &Object,
    copied: &mut HashMap<ObjectId, ObjectId>,
    depth: usize,
) -> Result<ObjectId> {
    /// Guard against a reference cycle in a malformed document.
    const MAX_DEPTH: usize = 32;

    if depth > MAX_DEPTH {
        return Err(PdfError::Internal(
            "the text layer's font structure is nested too deeply".into(),
        ));
    }

    let id = match object {
        Object::Reference(id) => *id,
        // A direct object still needs an identity in the target so that the
        // resource dictionary can point at it.
        direct => {
            let rewritten = rewrite_references(target, source, direct, copied, depth)?;
            return Ok(target.add_object(rewritten));
        }
    };

    if let Some(existing) = copied.get(&id) {
        return Ok(*existing);
    }

    let resolved = source
        .get_object(id)
        .map_err(|error| PdfError::Internal(format!("the text layer is incomplete: {error}")))?;

    // Reserve the id before recursing so that a cycle terminates.
    let new_id = target.new_object_id();
    copied.insert(id, new_id);

    let rewritten = rewrite_references(target, source, resolved, copied, depth + 1)?;
    target.set_object(new_id, rewritten);

    Ok(new_id)
}

/// Clone an object, replacing every reference with its copy in the target.
fn rewrite_references(
    target: &mut lopdf::Document,
    source: &lopdf::Document,
    object: &Object,
    copied: &mut HashMap<ObjectId, ObjectId>,
    depth: usize,
) -> Result<Object> {
    Ok(match object {
        Object::Reference(_) => {
            Object::Reference(deep_copy(target, source, object, copied, depth)?)
        }
        Object::Array(items) => {
            let mut rewritten = Vec::with_capacity(items.len());
            for item in items {
                rewritten.push(rewrite_references(target, source, item, copied, depth)?);
            }
            Object::Array(rewritten)
        }
        Object::Dictionary(dict) => {
            Object::Dictionary(rewrite_dictionary(target, source, dict, copied, depth)?)
        }
        Object::Stream(stream) => {
            let dict = rewrite_dictionary(target, source, &stream.dict, copied, depth)?;
            Object::Stream(Stream::new(dict, stream.content.clone()))
        }
        other => other.clone(),
    })
}

fn rewrite_dictionary(
    target: &mut lopdf::Document,
    source: &lopdf::Document,
    dict: &Dictionary,
    copied: &mut HashMap<ObjectId, ObjectId>,
    depth: usize,
) -> Result<Dictionary> {
    let mut rewritten = Dictionary::new();

    for (key, value) in dict.iter() {
        rewritten.set(
            key.clone(),
            rewrite_references(target, source, value, copied, depth)?,
        );
    }

    Ok(rewritten)
}

/// Point every `Tf` operator at the imported font's new name.
fn rewrite_font_names(content: Content, fonts: &FontImports) -> Vec<lopdf::content::Operation> {
    content
        .operations
        .into_iter()
        .map(|mut operation| {
            if operation.operator == "Tf" {
                if let Some(Object::Name(name)) = operation.operands.first() {
                    if let Some((new_name, _)) = fonts.get(name) {
                        operation.operands[0] = Object::Name(new_name.clone());
                    }
                }
            }
            operation
        })
        .collect()
}

/// Add a content stream to the end of a page's `/Contents`.
fn append_content(target: &mut lopdf::Document, page_id: ObjectId, layer: ObjectId) -> Result<()> {
    let page = target.get_object_mut(page_id)?.as_dict_mut()?;

    let contents = match page.get(b"Contents") {
        // Already a list: the new stream goes last so it draws on top.
        Ok(Object::Array(existing)) => {
            let mut array = existing.clone();
            array.push(Object::Reference(layer));
            array
        }
        Ok(Object::Reference(existing)) => {
            vec![Object::Reference(*existing), Object::Reference(layer)]
        }
        // A page with no content at all is legal; the text becomes all of it.
        _ => vec![Object::Reference(layer)],
    };

    page.set("Contents", Object::Array(contents));

    Ok(())
}

/// Add the imported fonts to the page's own resource dictionary.
fn merge_fonts(target: &mut lopdf::Document, page_id: ObjectId, fonts: &FontImports) -> Result<()> {
    if fonts.is_empty() {
        return Ok(());
    }

    // Resources are inheritable, so a page may not have its own. Materialising
    // a copy on the page is correct and avoids editing a dictionary shared with
    // every other page in the document.
    let inherited = page_resources(target, page_id).unwrap_or_default();
    let mut resources = inherited;

    let mut font_dict = resources
        .get(b"Font")
        .ok()
        .and_then(|object| resolve_dictionary(target, object))
        .unwrap_or_default();

    for (new_name, id) in fonts.values() {
        font_dict.set(new_name.clone(), Object::Reference(*id));
    }

    resources.set("Font", Object::Dictionary(font_dict));

    let resources_id = target.add_object(Object::Dictionary(resources));
    let page = target.get_object_mut(page_id)?.as_dict_mut()?;
    page.set("Resources", Object::Reference(resources_id));

    Ok(())
}

/// A page's `/Resources`, following the page tree upwards.
fn page_resources(doc: &lopdf::Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut current = Some(page_id);

    for _ in 0..64 {
        let id = current?;
        let dict = doc.get_dictionary(id).ok()?;

        if let Ok(resources) = dict.get(b"Resources") {
            return resolve_dictionary(doc, resources);
        }

        current = dict
            .get(b"Parent")
            .ok()
            .and_then(|object| object.as_reference().ok());
    }

    None
}

fn resolve_dictionary(doc: &lopdf::Document, object: &Object) -> Option<Dictionary> {
    match object {
        Object::Dictionary(dict) => Some(dict.clone()),
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::Operation;
    use lopdf::dictionary;

    fn content(operators: &[&str]) -> Content {
        Content {
            operations: operators
                .iter()
                .map(|operator| Operation::new(operator, vec![]))
                .collect(),
        }
    }

    #[test]
    fn a_stream_that_draws_glyphs_is_recognised() {
        assert!(draws_any_text(&content(&["BT", "Tj", "ET"])));
        assert!(draws_any_text(&content(&["TJ"])));
        assert!(draws_any_text(&content(&["'"])));
    }

    #[test]
    fn a_stream_with_no_glyphs_is_recognised_as_empty() {
        assert!(!draws_any_text(&content(&["BT", "ET"])));
        assert!(!draws_any_text(&content(&[])));
        assert!(!draws_any_text(&content(&["q", "cm", "Q"])));
    }

    #[test]
    fn font_operands_are_repointed() {
        let mut fonts = HashMap::new();
        fonts.insert(b"F1".to_vec(), (b"FsOcr0".to_vec(), (9, 0)));

        let operations = rewrite_font_names(
            Content {
                operations: vec![Operation::new(
                    "Tf",
                    vec![Object::Name(b"F1".to_vec()), 12.into()],
                )],
            },
            &fonts,
        );

        assert_eq!(operations[0].operands[0], Object::Name(b"FsOcr0".to_vec()));
    }

    #[test]
    fn an_unknown_font_operand_is_left_alone() {
        let operations = rewrite_font_names(
            Content {
                operations: vec![Operation::new(
                    "Tf",
                    vec![Object::Name(b"Unmapped".to_vec()), 12.into()],
                )],
            },
            &HashMap::new(),
        );

        assert_eq!(
            operations[0].operands[0],
            Object::Name(b"Unmapped".to_vec())
        );
    }

    #[test]
    fn a_single_content_stream_becomes_an_array() {
        let mut doc = lopdf::Document::with_version("1.5");
        let existing = doc.add_object(Stream::new(Dictionary::new(), b"BT ET".to_vec()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => existing,
        });

        append_content(&mut doc, page, (99, 0)).unwrap();

        let contents = doc.get_dictionary(page).unwrap().get(b"Contents").unwrap();
        let array = contents.as_array().unwrap();
        assert_eq!(array.len(), 2);
        // Last, so the text draws over the page rather than under it.
        assert_eq!(array[1], Object::Reference((99, 0)));
    }

    #[test]
    fn an_existing_array_is_extended() {
        let mut doc = lopdf::Document::with_version("1.5");
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => vec![Object::Reference((1, 0)), Object::Reference((2, 0))],
        });

        append_content(&mut doc, page, (99, 0)).unwrap();

        let contents = doc.get_dictionary(page).unwrap().get(b"Contents").unwrap();
        assert_eq!(contents.as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_page_without_content_gets_some() {
        let mut doc = lopdf::Document::with_version("1.5");
        let page = doc.add_object(dictionary! { "Type" => "Page" });

        append_content(&mut doc, page, (99, 0)).unwrap();

        let contents = doc.get_dictionary(page).unwrap().get(b"Contents").unwrap();
        assert_eq!(contents.as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_shared_object_is_copied_once() {
        let mut source = lopdf::Document::with_version("1.5");
        let shared = source.add_object(dictionary! { "Type" => "FontDescriptor" });
        let first = source.add_object(dictionary! {
            "Type" => "Font",
            "FontDescriptor" => shared,
        });
        let second = source.add_object(dictionary! {
            "Type" => "Font",
            "FontDescriptor" => shared,
        });

        let mut target = lopdf::Document::with_version("1.5");
        let mut copied = HashMap::new();

        let a = deep_copy(
            &mut target,
            &source,
            &Object::Reference(first),
            &mut copied,
            0,
        )
        .unwrap();
        let b = deep_copy(
            &mut target,
            &source,
            &Object::Reference(second),
            &mut copied,
            0,
        )
        .unwrap();

        assert_ne!(a, b, "the two fonts are distinct");

        let descriptor_of = |id| {
            target
                .get_dictionary(id)
                .unwrap()
                .get(b"FontDescriptor")
                .unwrap()
                .as_reference()
                .unwrap()
        };
        assert_eq!(
            descriptor_of(a),
            descriptor_of(b),
            "the shared descriptor should have been copied once"
        );
    }

    #[test]
    fn resources_are_found_through_the_page_tree() {
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let resources = doc.add_object(dictionary! { "Font" => Dictionary::new() });
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page.into()],
                "Resources" => resources,
            }),
        );

        assert!(page_resources(&doc, page).is_some());
    }
}
