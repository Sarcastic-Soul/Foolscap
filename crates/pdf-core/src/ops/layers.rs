//! Optional content (layers): list them, and show or hide one in the default
//! configuration.
//!
//! Layers are the optional content groups in `/OCProperties /OCGs`. A group is
//! hidden when its reference appears in the default configuration's `/OFF`
//! array, which is `/OCProperties /D` (reached directly or through a
//! reference).

use lopdf::{Dictionary, Object, ObjectId};

use crate::document::Document;
use crate::error::{PdfError, Result};

/// One optional content group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub name: String,
    /// Whether the default configuration shows it.
    pub visible: bool,
}

/// List the layers. A document without optional content has none.
pub fn list_layers(doc: &Document) -> Result<Vec<Layer>> {
    let inner = &doc.inner;
    let hidden = hidden_groups(inner);
    Ok(groups(inner)
        .into_iter()
        .map(|(id, name)| Layer {
            name,
            visible: !hidden.contains(&id),
        })
        .collect())
}

/// Show or hide the layer called `name` in the default configuration.
pub fn set_layer_visibility(doc: &mut Document, name: &str, visible: bool) -> Result<()> {
    let inner = &mut doc.inner;
    let all = groups(inner);
    if all.is_empty() {
        return Err(PdfError::NoLayers);
    }
    let group = all
        .iter()
        .find(|(_, group)| group == name)
        .map(|(id, _)| *id)
        .ok_or_else(|| PdfError::NoSuchLayer(name.to_string()))?;

    // Work on copies, then write each back where it came from, so that a
    // configuration held by reference is replaced in place.
    let props_ref = inner
        .catalog()?
        .get(b"OCProperties")
        .ok()
        .and_then(|object| object.as_reference().ok());
    let mut props = match props_ref {
        Some(id) => inner.get_dictionary(id)?.clone(),
        None => inner.catalog()?.get(b"OCProperties")?.as_dict()?.clone(),
    };

    let config_ref = props
        .get(b"D")
        .ok()
        .and_then(|object| object.as_reference().ok());
    let mut config = match config_ref {
        Some(id) => inner.get_dictionary(id)?.clone(),
        None => props.get(b"D")?.as_dict()?.clone(),
    };

    let mut off: Vec<Object> = config
        .get(b"OFF")
        .ok()
        .and_then(|object| object.as_array().ok())
        .cloned()
        .unwrap_or_default();
    off.retain(|object| object.as_reference().ok() != Some(group));
    if !visible {
        off.push(Object::Reference(group));
    }
    config.set("OFF", Object::Array(off));

    match config_ref {
        Some(id) => *inner.get_object_mut(id)? = Object::Dictionary(config),
        None => props.set("D", Object::Dictionary(config)),
    }
    match props_ref {
        Some(id) => *inner.get_object_mut(id)? = Object::Dictionary(props),
        None => inner
            .catalog_mut()?
            .set("OCProperties", Object::Dictionary(props)),
    }
    Ok(())
}

/// Every optional content group with its display name, in `/OCGs` order.
fn groups(inner: &lopdf::Document) -> Vec<(ObjectId, String)> {
    let Some(props) = properties(inner) else {
        return Vec::new();
    };
    let Some(list) = props
        .get(b"OCGs")
        .ok()
        .and_then(|object| inner.dereference(object).ok())
        .and_then(|(_, object)| object.as_array().ok())
    else {
        return Vec::new();
    };

    list.iter()
        .filter_map(|entry| {
            let id = entry.as_reference().ok()?;
            let group = inner.get_dictionary(id).ok()?;
            let name = group.get(b"Name").ok()?;
            match name {
                Object::String(bytes, _) => Some((id, String::from_utf8_lossy(bytes).into_owned())),
                _ => None,
            }
        })
        .collect()
}

/// The groups hidden by the default configuration.
fn hidden_groups(inner: &lopdf::Document) -> Vec<ObjectId> {
    let Some(config) = properties(inner).and_then(|props| dict_entry(inner, props, b"D")) else {
        return Vec::new();
    };
    config
        .get(b"OFF")
        .ok()
        .and_then(|object| inner.dereference(object).ok())
        .and_then(|(_, object)| object.as_array().ok())
        .map(|array| array.iter().filter_map(|o| o.as_reference().ok()).collect())
        .unwrap_or_default()
}

/// The `/OCProperties` dictionary, direct or by reference.
fn properties(inner: &lopdf::Document) -> Option<&Dictionary> {
    let catalog = inner.catalog().ok()?;
    dict_at(inner, catalog.get(b"OCProperties").ok()?)
}

/// A dictionary entry that is itself a dictionary, direct or by reference.
fn dict_entry<'a>(
    inner: &'a lopdf::Document,
    dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Dictionary> {
    dict_at(inner, dict.get(key).ok()?)
}

fn dict_at<'a>(inner: &'a lopdf::Document, object: &'a Object) -> Option<&'a Dictionary> {
    inner.dereference(object).ok()?.1.as_dict().ok()
}
