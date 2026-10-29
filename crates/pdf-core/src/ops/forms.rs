//! Interactive form (AcroForm) filling.
//!
//! The catalog's `/AcroForm /Fields` array is the entry point. Each entry is a
//! field dictionary; `/T` holds the partial name and fully-qualified names are
//! the partials joined with dots, so `contact` + `email` is addressable as
//! `contact.email`. A field's `/Kids` are either nested fields (they carry
//! their own `/T`) or the widget annotations that draw it (they do not).
//!
//! Filling writes `/V` on the field dictionary — a string for text and choice
//! fields, a name for buttons — and `/AS` on every widget that has a state
//! dictionary under `/AP /N`, so the right appearance shows before any viewer
//! regenerates one. `/NeedAppearances` is then set on the AcroForm so viewers
//! rebuild the appearances of text fields, whose values cannot be rendered
//! without a font to measure with. Signature fields are refused: a value
//! written over a signature would break it.
//!
//! Everything that can be foreseen is reported rather than raised: names that
//! match no field go to [`FillFormReport::not_found`], and values a field
//! cannot take (a signature, a choice missing from `/Opt`, a string for a
//! checkbox) go to [`FillFormReport::unsupported`] as `name: reason`.

use std::collections::BTreeMap;

use lopdf::{Dictionary, Object, ObjectId, StringFormat};
use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::text;

/// The value to write into a field. A JSON boolean addresses a checkbox or
/// radio field; a string covers text and choice fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldValue {
    Checkbox(bool),
    Text(String),
}

/// What happened to each requested field.
///
/// `unsupported` entries are formatted `name: reason`, so the reason a value
/// was refused travels with the report.
#[derive(Debug, Default, Serialize)]
pub struct FillFormReport {
    pub filled: Vec<String>,
    pub not_found: Vec<String>,
    pub unsupported: Vec<String>,
}

/// How deeply to follow `/Kids` before deciding a form is malformed. Real
/// nesting is two or three levels; this only exists to stop a cycle.
const MAX_DEPTH: usize = 32;

/// `/Ff` bit 3: a pushbutton, which holds no value.
const FLAG_PUSHBUTTON: i64 = 1 << 2;
/// `/Ff` bit 16: a radio button, whose value is the chosen state's name.
const FLAG_RADIO: i64 = 1 << 15;

/// The name every button's un-set state carries, by definition in the
/// specification.
const OFF: &[u8] = b"Off";

/// The on-state to write when a checkbox declares no appearance states at all.
/// The one producers use by convention.
const DEFAULT_ON_STATE: &[u8] = b"Yes";

/// Where the `/AcroForm` dictionary lives, so `/NeedAppearances` can be set on
/// the same object the fields were read from.
enum AcroForm {
    /// Its own object, the usual case.
    Indirect(ObjectId),
    /// Inline in the catalog.
    Direct,
}

/// One terminal field, gathered before anything is written.
struct FormField {
    /// The field dictionary followed by its ancestors, nearest first. `/FT`,
    /// `/Ff` and `/Opt` are inheritable, so lookups walk this list.
    chain: Vec<ObjectId>,
    /// Every dictionary that draws this field — the merged field/widget
    /// dictionary itself, and the `/T`-less widget kids — paired with the
    /// state names in its `/AP /N`, when that entry is a state dictionary.
    /// Widgets with a single `/AP /N` stream (text fields) have no states and
    /// no `/AS` to set, and appear here as an empty list.
    widgets: Vec<(ObjectId, Vec<Vec<u8>>)>,
}

/// Read the JSON that the `fill-form` command and the GUI take: an object of
/// `"field name": value` pairs. A boolean fills a checkbox and a string fills a
/// text or choice field.
///
/// Any other value is refused by naming the field that carries it. An untagged
/// enum would otherwise fail with a generic "no variant matched" message.
pub fn parse_form_data(json: &str) -> Result<BTreeMap<String, FieldValue>> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| PdfError::InvalidFormData(format!("not valid JSON: {error}")))?;

    let serde_json::Value::Object(map) = value else {
        return Err(PdfError::InvalidFormData(
            "the data must be a JSON object of \"field name\": value pairs".into(),
        ));
    };

    let mut data = BTreeMap::new();
    for (name, value) in map {
        let value = match value {
            serde_json::Value::Bool(on) => FieldValue::Checkbox(on),
            serde_json::Value::String(text) => FieldValue::Text(text),
            other => {
                return Err(PdfError::InvalidFormData(format!(
                    "field \"{name}\": expected a string or a boolean, found {}",
                    json_kind(&other)
                )))
            }
        };
        data.insert(name, value);
    }
    Ok(data)
}

/// What a JSON value of the wrong kind is called, in a message a person reads.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
        serde_json::Value::Bool(_) | serde_json::Value::String(_) => "a valid value",
    }
}

/// Write `data` into the document's form fields, setting `/V` and marking
/// appearances for regeneration.
///
/// A document with no `/AcroForm` is not an error: every requested name simply
/// lands in `not_found`.
pub fn fill_form(
    doc: &mut Document,
    data: &BTreeMap<String, FieldValue>,
) -> Result<FillFormReport> {
    let (acro_form, fields) = scan(&doc.inner);

    let mut report = FillFormReport::default();
    for (name, value) in data {
        match fields.get(name) {
            None => report.not_found.push(name.clone()),
            Some(field) => match apply_value(&mut doc.inner, field, value) {
                Ok(()) => report.filled.push(name.clone()),
                Err(reason) => report.unsupported.push(format!("{name}: {reason}")),
            },
        }
    }

    if !report.filled.is_empty() {
        set_need_appearances(&mut doc.inner, &acro_form)?;
    }

    Ok(report)
}

/// Collect every addressable field, plus where the AcroForm lives.
fn scan(doc: &lopdf::Document) -> (Option<AcroForm>, BTreeMap<String, FormField>) {
    let mut fields = BTreeMap::new();

    let Some(root) = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| root.as_reference().ok())
    else {
        return (None, fields);
    };

    let Some(acro_object) = doc
        .get_dictionary(root)
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok())
    else {
        return (None, fields);
    };

    let Ok((id, object)) = doc.dereference(acro_object) else {
        return (None, fields);
    };
    let Ok(form) = object.as_dict() else {
        return (None, fields);
    };
    let place = match id {
        Some(id) => AcroForm::Indirect(id),
        None => AcroForm::Direct,
    };

    let entries = form
        .get(b"Fields")
        .ok()
        .and_then(|object| doc.dereference(object).ok())
        .and_then(|(_, entries)| entries.as_array().ok());
    if let Some(entries) = entries {
        collect_fields(doc, entries, "", &[], 0, &mut fields);
    }

    (Some(place), fields)
}

/// Walk a `/Fields` array (or one field's `/Kids`), registering every field
/// that carries a `/T` under its fully-qualified name.
fn collect_fields(
    doc: &lopdf::Document,
    entries: &[Object],
    prefix: &str,
    ancestors: &[ObjectId],
    depth: usize,
    fields: &mut BTreeMap<String, FormField>,
) {
    if depth > MAX_DEPTH {
        tracing::warn!("form field nesting exceeds {MAX_DEPTH} levels; the rest is ignored");
        return;
    }

    for entry in entries {
        // Only indirect fields can be written back to; a direct dictionary
        // here would have to be rebuilt through its parent, and real
        // producers always emit references.
        let Some((id, dict)) = resolve(doc, entry) else {
            continue;
        };
        let Some(id) = id else { continue };

        let Some(partial) = partial_name(dict) else {
            // No /T: a widget belonging to the enclosing field, which
            // registered it when it saw the kid. Nothing to address here.
            continue;
        };

        let qualified = if prefix.is_empty() {
            partial
        } else {
            format!("{prefix}.{partial}")
        };
        let chain = std::iter::once(id)
            .chain(ancestors.iter().copied())
            .collect::<Vec<_>>();

        // Kids with a /T of their own are nested fields; kids without one are
        // this field's widgets.
        let mut widgets = vec![id];
        let kids = dict
            .get(b"Kids")
            .ok()
            .and_then(|object| doc.dereference(object).ok())
            .and_then(|(_, kids)| kids.as_array().ok());
        if let Some(kids) = kids {
            for kid in kids {
                let Some((kid_id, kid_dict)) = resolve(doc, kid) else {
                    continue;
                };
                if partial_name(kid_dict).is_some() {
                    collect_fields(
                        doc,
                        std::slice::from_ref(kid),
                        &qualified,
                        &chain,
                        depth + 1,
                        fields,
                    );
                } else if let Some(kid_id) = kid_id {
                    widgets.push(kid_id);
                }
            }
        }

        // A dictionary with a single /AP /N stream has one appearance, not a
        // set of states, and takes no /AS.
        let widgets = widgets
            .into_iter()
            .filter_map(|id| {
                let states = doc
                    .get_dictionary(id)
                    .ok()
                    .and_then(|dict| appearance_states(doc, dict));
                Some((id, states?))
            })
            .collect::<Vec<_>>();

        fields
            .entry(qualified)
            .or_insert(FormField { chain, widgets });
    }
}

/// A field's `/T`, decoded. `None` for widget kids, which carry none.
fn partial_name(dict: &Dictionary) -> Option<String> {
    match dict.get(b"T").ok()? {
        Object::String(bytes, _) => Some(text::decode_pdf_string(bytes)),
        _ => None,
    }
}

/// Resolve an object to its dictionary, following a reference. Returns the
/// object id when the dictionary is indirect.
fn resolve<'a>(
    doc: &'a lopdf::Document,
    object: &'a Object,
) -> Option<(Option<ObjectId>, &'a Dictionary)> {
    let (id, resolved) = doc.dereference(object).ok()?;
    resolved.as_dict().ok().map(|dict| (id, dict))
}

/// The state names in a dictionary's `/AP /N`, when that entry is a state
/// dictionary rather than a single appearance stream.
fn appearance_states(doc: &lopdf::Document, dict: &Dictionary) -> Option<Vec<Vec<u8>>> {
    let (_, ap) = resolve(doc, dict.get(b"AP").ok()?)?;
    let (_, normal) = doc.dereference(ap.get(b"N").ok()?).ok()?;
    let states = normal.as_dict().ok()?;
    Some(states.iter().map(|(name, _)| name.clone()).collect())
}

/// The first inheritable entry found walking a field's chain, self first.
fn inherited(doc: &lopdf::Document, chain: &[ObjectId], key: &[u8]) -> Option<Object> {
    chain
        .iter()
        .find_map(|id| doc.get_dictionary(*id).ok()?.get(key).ok().cloned())
}

/// Write one value into one field. `Err` carries the reason for the report.
fn apply_value(
    doc: &mut lopdf::Document,
    field: &FormField,
    value: &FieldValue,
) -> std::result::Result<(), String> {
    let field_type = inherited(doc, &field.chain, b"FT")
        .and_then(|object| object.as_name().ok().map(<[u8]>::to_vec));
    let flags = inherited(doc, &field.chain, b"Ff")
        .and_then(|object| object.as_i64().ok())
        .unwrap_or(0);

    match field_type.as_deref() {
        Some(t) if t == b"Tx".as_slice() => fill_text(doc, field, value),
        Some(t) if t == b"Btn".as_slice() => fill_button(doc, field, value, flags),
        Some(t) if t == b"Ch".as_slice() => fill_choice(doc, field, value),
        Some(t) if t == b"Sig".as_slice() => Err("signature fields cannot be filled".to_string()),
        Some(t) => Err(format!(
            "unsupported field type {}",
            String::from_utf8_lossy(t)
        )),
        None => Err("the field has no /FT".to_string()),
    }
}

/// `/Tx`: the value is the field's new text.
fn fill_text(
    doc: &mut lopdf::Document,
    field: &FormField,
    value: &FieldValue,
) -> std::result::Result<(), String> {
    let FieldValue::Text(text) = value else {
        return Err("a text field expects a string value".to_string());
    };

    let bytes = text::encode_pdf_string(text);
    // Literal keeps simple values readable in a hex dump; the UTF-16BE that
    // non-ASCII becomes is safer hex-encoded, where no byte needs escaping.
    let format = if text.is_ascii() {
        StringFormat::Literal
    } else {
        StringFormat::Hexadecimal
    };
    set_entry(doc, field.chain[0], b"V", Object::String(bytes, format))
}

/// `/Btn`: a checkbox takes a boolean, a radio button or a named-state button
/// takes the state's name as a string.
fn fill_button(
    doc: &mut lopdf::Document,
    field: &FormField,
    value: &FieldValue,
    flags: i64,
) -> std::result::Result<(), String> {
    if flags & FLAG_PUSHBUTTON != 0 {
        return Err("pushbutton fields have no value to set".to_string());
    }

    let field_id = field.chain[0];
    match value {
        FieldValue::Text(choice) => {
            // A string names the state to select, as a radio group's choice.
            let known = known_states(field);
            if !known.is_empty() && !known.iter().any(|state| state == choice.as_bytes()) {
                let names = known
                    .iter()
                    .map(|state| String::from_utf8_lossy(state).into_owned())
                    .collect::<Vec<_>>();
                return Err(format!(
                    "\"{choice}\" is not one of the choices ({})",
                    list_names(&names)
                ));
            }

            set_entry(
                doc,
                field_id,
                b"V",
                Object::Name(choice.as_bytes().to_vec()),
            )?;
            set_widget_states(doc, field, choice.as_bytes())
        }
        FieldValue::Checkbox(on) => {
            if *on && flags & FLAG_RADIO != 0 {
                return Err("a radio button needs its choice name as a string".to_string());
            }

            let state = if *on { on_state(field) } else { OFF.to_vec() };
            set_entry(doc, field_id, b"V", Object::Name(state.clone()))?;
            set_widget_states(doc, field, &state)
        }
    }
}

/// `/Ch`: the value must be one of the `/Opt` export values, when the field
/// declares any.
fn fill_choice(
    doc: &mut lopdf::Document,
    field: &FormField,
    value: &FieldValue,
) -> std::result::Result<(), String> {
    let FieldValue::Text(text) = value else {
        return Err("a choice field expects a string value".to_string());
    };

    if let Some(options) = opt_exports(doc, &field.chain) {
        if !options.iter().any(|option| option == text) {
            return Err(format!(
                "\"{text}\" is not one of the options ({})",
                list_names(&options)
            ));
        }
    }

    let bytes = text::encode_pdf_string(text);
    let format = if text.is_ascii() {
        StringFormat::Literal
    } else {
        StringFormat::Hexadecimal
    };
    set_entry(doc, field.chain[0], b"V", Object::String(bytes, format))
}

/// A button field's selectable states: every state name but `Off`.
fn known_states(field: &FormField) -> Vec<Vec<u8>> {
    let mut known: Vec<Vec<u8>> = Vec::new();
    for (_, states) in &field.widgets {
        for state in states {
            if state != OFF && !known.contains(state) {
                known.push(state.clone());
            }
        }
    }
    known
}

/// The on-state a checkbox set to true should show: the first non-`Off` state
/// any of its widgets declares, or `Yes` when none does.
fn on_state(field: &FormField) -> Vec<u8> {
    field
        .widgets
        .iter()
        .flat_map(|(_, states)| states.iter())
        .find(|state| state.as_slice() != OFF)
        .cloned()
        .unwrap_or_else(|| DEFAULT_ON_STATE.to_vec())
}

/// The export values in a field's `/Opt`: a plain string is its own export
/// value, a two-element array pairs one with its display label.
fn opt_exports(doc: &lopdf::Document, chain: &[ObjectId]) -> Option<Vec<String>> {
    let object = inherited(doc, chain, b"Opt")?;
    let (_, entries) = doc.dereference(&object).ok()?;
    let entries = entries.as_array().ok()?;

    let mut exports = Vec::with_capacity(entries.len());
    for entry in entries {
        let Ok((_, resolved)) = doc.dereference(entry) else {
            continue;
        };
        match resolved {
            Object::String(bytes, _) => exports.push(text::decode_pdf_string(bytes)),
            Object::Array(pair) => {
                if let Some(Object::String(bytes, _)) = pair.first() {
                    exports.push(text::decode_pdf_string(bytes));
                }
            }
            _ => {}
        }
    }
    Some(exports)
}

/// Set `/AS` on every state-bearing widget of a button field: the named state
/// where the widget offers it, `Off` everywhere else — the other buttons in a
/// radio group must not stay on.
fn set_widget_states(
    doc: &mut lopdf::Document,
    field: &FormField,
    state: &[u8],
) -> std::result::Result<(), String> {
    for (id, states) in &field.widgets {
        let shown = if states.iter().any(|name| name == state) {
            state
        } else {
            OFF
        };
        set_entry(doc, *id, b"AS", Object::Name(shown.to_vec()))?;
    }
    Ok(())
}

/// Overwrite one dictionary entry. A failure here means the object stopped
/// matching what the scan read; the caller prefixes the field's name.
fn set_entry(
    doc: &mut lopdf::Document,
    id: ObjectId,
    key: &[u8],
    value: Object,
) -> std::result::Result<(), String> {
    doc.get_object_mut(id)
        .map_err(|error| format!("could not update the field: {error}"))?
        .as_dict_mut()
        .map_err(|error| format!("could not update the field: {error}"))?
        .set(key.to_vec(), value);
    Ok(())
}

/// Set `/NeedAppearances` on the AcroForm so viewers rebuild appearance
/// streams for the new values.
fn set_need_appearances(doc: &mut lopdf::Document, place: &Option<AcroForm>) -> Result<()> {
    match place {
        Some(AcroForm::Indirect(id)) => doc
            .get_dictionary_mut(*id)?
            .set("NeedAppearances", Object::Boolean(true)),
        Some(AcroForm::Direct) => {
            let root = doc
                .trailer
                .get(b"Root")
                .and_then(Object::as_reference)
                .map_err(|error| PdfError::Internal(format!("catalog unreadable: {error}")))?;
            doc.get_object_mut(root)?
                .as_dict_mut()?
                .get_mut(b"AcroForm")?
                .as_dict_mut()?
                .set("NeedAppearances", Object::Boolean(true));
        }
        None => tracing::warn!("fields were filled but no AcroForm dictionary was found"),
    }
    Ok(())
}

/// Names joined for a report message, capped so a long option list cannot
/// produce an unreadable one-liner.
fn list_names(names: &[String]) -> String {
    const CAP: usize = 8;
    let mut shown: Vec<&str> = names.iter().take(CAP).map(String::as_str).collect();
    if names.len() > CAP {
        shown.push("…");
    }
    shown.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget(states: &[&[u8]]) -> FormField {
        let id = (1, 0);
        FormField {
            chain: vec![id],
            widgets: vec![(id, states.iter().map(|s| s.to_vec()).collect())],
        }
    }

    #[test]
    fn the_on_state_is_the_first_state_that_is_not_off() {
        assert_eq!(on_state(&widget(&[b"Off", b"Chk"])), b"Chk".to_vec());
        assert_eq!(on_state(&widget(&[b"Yes"])), b"Yes".to_vec());
    }

    #[test]
    fn a_button_with_no_states_falls_back_to_yes() {
        assert_eq!(on_state(&widget(&[])), b"Yes".to_vec());
        assert_eq!(on_state(&widget(&[b"Off"])), b"Yes".to_vec());
    }

    #[test]
    fn known_states_skip_off_and_deduplicate() {
        let field = FormField {
            chain: vec![(1, 0)],
            widgets: vec![
                ((1, 0), vec![b"Off".to_vec(), b"Red".to_vec()]),
                ((2, 0), vec![b"Red".to_vec(), b"Blue".to_vec()]),
            ],
        };
        assert_eq!(known_states(&field), [b"Red".to_vec(), b"Blue".to_vec()]);
    }

    #[test]
    fn name_lists_are_capped() {
        let names: Vec<String> = (0..12).map(|n| format!("opt{n}")).collect();
        let listed = list_names(&names);
        assert!(listed.starts_with("opt0, opt1"));
        assert!(listed.ends_with("…"));
        assert_eq!(list_names(&["one".to_string()]), "one");
    }
}
