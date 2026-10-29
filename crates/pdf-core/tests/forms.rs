//! AcroForm filling: fixtures are built with lopdf in the test, covering the
//! field shapes producers actually write — merged field/widget dictionaries,
//! nested `parent.child` names, checkboxes with split widget kids, radio
//! groups and choice fields — then filled, saved and reloaded so `/V` and
//! `/AS` are read back from a real file.

mod common;

use std::collections::BTreeMap;

use common::{appearance_stream, Attributes, Workspace, A4};
use lopdf::{dictionary, Dictionary, Document, Object};
use pdf_core::{fill_form, FieldValue, FillFormReport};

/// The `/Ff` bit that marks a radio button.
const RADIO_FLAG: i64 = 1 << 15;

// ------------------------------------------------------------------ fixtures

/// A one-page A4 document whose catalog points at a fresh, empty AcroForm.
fn form_document() -> (Document, lopdf::ObjectId, lopdf::ObjectId) {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), A4.0.into(), A4.1.into()],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => 1,
            "Kids" => vec![Object::Reference(page_id)],
        }),
    );

    let acroform_id = doc.add_object(dictionary! {
        "Fields" => Vec::<Object>::new(),
        // Explicitly false, so a test can prove filling flips it.
        "NeedAppearances" => false,
    });
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);

    (doc, acroform_id, page_id)
}

fn rect(x: i64, y: i64, width: i64, height: i64) -> Vec<Object> {
    vec![x.into(), y.into(), (x + width).into(), (y + height).into()]
}

/// Append `referenced` to the array `holder[key]`, creating it if absent.
fn push_reference(
    doc: &mut Document,
    holder: lopdf::ObjectId,
    key: &[u8],
    referenced: lopdf::ObjectId,
) {
    let dict = doc.get_dictionary_mut(holder).unwrap();
    let mut items = match dict.get(key) {
        Ok(Object::Array(existing)) => existing.clone(),
        _ => Vec::new(),
    };
    items.push(Object::Reference(referenced));
    dict.set(key.to_vec(), Object::Array(items));
}

/// A text field merged with its widget, the common producer shape.
fn add_text_field(
    doc: &mut Document,
    acroform_id: lopdf::ObjectId,
    page_id: lopdf::ObjectId,
    name: &str,
) -> lopdf::ObjectId {
    let id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "T" => Object::string_literal(name),
        "V" => Object::string_literal(""),
        "Rect" => rect(72, 700, 228, 24),
    });
    push_reference(doc, acroform_id, b"Fields", id);
    push_reference(doc, page_id, b"Annots", id);
    id
}

/// A `parent.child` text field: the parent names a group, the child is the
/// widget.
fn add_nested_text(doc: &mut Document, acroform_id: lopdf::ObjectId, page_id: lopdf::ObjectId) {
    let parent_id = doc.new_object_id();
    let child_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "Parent" => parent_id,
        "FT" => "Tx",
        "T" => Object::string_literal("email"),
        "V" => Object::string_literal(""),
        "Rect" => rect(72, 660, 228, 24),
    });
    doc.objects.insert(
        parent_id,
        Object::Dictionary(dictionary! {
            "T" => Object::string_literal("contact"),
            "Kids" => vec![Object::Reference(child_id)],
        }),
    );
    push_reference(doc, acroform_id, b"Fields", parent_id);
    push_reference(doc, page_id, b"Annots", child_id);
}

/// A checkbox merged with its widget, showing one of `on_state`/`Off`.
fn add_checkbox(
    doc: &mut Document,
    acroform_id: lopdf::ObjectId,
    page_id: lopdf::ObjectId,
    name: &str,
    on_state: &str,
) -> lopdf::ObjectId {
    let on = appearance_stream(doc, 20, 20, "checked box");
    let off = appearance_stream(doc, 20, 20, "empty box");
    let id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Btn",
        "T" => Object::string_literal(name),
        "V" => Object::Name(b"Off".to_vec()),
        "AP" => dictionary! { "N" => dictionary! { on_state => on, "Off" => off } },
        "AS" => Object::Name(b"Off".to_vec()),
        "Rect" => rect(72, 620, 20, 20),
    });
    push_reference(doc, acroform_id, b"Fields", id);
    push_reference(doc, page_id, b"Annots", id);
    id
}

/// A checkbox whose field dictionary and widget annotation are separate
/// objects, joined by the widget's `/Parent`.
fn add_split_checkbox(
    doc: &mut Document,
    acroform_id: lopdf::ObjectId,
    page_id: lopdf::ObjectId,
    name: &str,
    on_state: &str,
) -> (lopdf::ObjectId, lopdf::ObjectId) {
    let field_id = doc.new_object_id();
    let on = appearance_stream(doc, 20, 20, "checked box");
    let off = appearance_stream(doc, 20, 20, "empty box");
    let widget_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "Parent" => field_id,
        "AP" => dictionary! { "N" => dictionary! { on_state => on, "Off" => off } },
        "AS" => Object::Name(b"Off".to_vec()),
        "Rect" => rect(72, 580, 20, 20),
    });
    doc.objects.insert(
        field_id,
        Object::Dictionary(dictionary! {
            "FT" => "Btn",
            "T" => Object::string_literal(name),
            "V" => Object::Name(b"Off".to_vec()),
            "Kids" => vec![Object::Reference(widget_id)],
        }),
    );
    push_reference(doc, acroform_id, b"Fields", field_id);
    push_reference(doc, page_id, b"Annots", widget_id);
    (field_id, widget_id)
}

/// A radio group: one field, one widget per choice.
fn add_radio(
    doc: &mut Document,
    acroform_id: lopdf::ObjectId,
    page_id: lopdf::ObjectId,
    name: &str,
    choices: &[&str],
) -> lopdf::ObjectId {
    let field_id = doc.new_object_id();
    let mut widget_ids = Vec::new();
    for choice in choices {
        let on = appearance_stream(doc, 20, 20, choice);
        let off = appearance_stream(doc, 20, 20, "empty box");
        let widget_id = doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Widget",
            "Parent" => field_id,
            "AP" => dictionary! { "N" => dictionary! { *choice => on, "Off" => off } },
            "AS" => Object::Name(b"Off".to_vec()),
            "Rect" => rect(72, 540, 20, 20),
        });
        widget_ids.push(widget_id);
    }
    doc.objects.insert(
        field_id,
        Object::Dictionary(dictionary! {
            "FT" => "Btn",
            "T" => Object::string_literal(name),
            "Ff" => RADIO_FLAG,
            "V" => Object::Name(b"Off".to_vec()),
            "Kids" => widget_ids
                .iter()
                .map(|id| Object::Reference(*id))
                .collect::<Vec<_>>(),
        }),
    );
    push_reference(doc, acroform_id, b"Fields", field_id);
    for widget_id in widget_ids {
        push_reference(doc, page_id, b"Annots", widget_id);
    }
    field_id
}

/// A choice (combo) field whose `/Opt` mixes plain exports with
/// export/label pairs.
fn add_choice(
    doc: &mut Document,
    acroform_id: lopdf::ObjectId,
    page_id: lopdf::ObjectId,
    name: &str,
    options: Vec<Object>,
) -> lopdf::ObjectId {
    let id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Ch",
        "T" => Object::string_literal(name),
        "Opt" => Object::Array(options),
        "V" => Object::string_literal(""),
        "Rect" => rect(72, 500, 228, 24),
    });
    push_reference(doc, acroform_id, b"Fields", id);
    push_reference(doc, page_id, b"Annots", id);
    id
}

/// A signature field.
fn add_signature(
    doc: &mut Document,
    acroform_id: lopdf::ObjectId,
    page_id: lopdf::ObjectId,
    name: &str,
) -> lopdf::ObjectId {
    let id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Sig",
        "T" => Object::string_literal(name),
        "Rect" => rect(72, 460, 228, 40),
    });
    push_reference(doc, acroform_id, b"Fields", id);
    push_reference(doc, page_id, b"Annots", id);
    id
}

/// The full fixture: one of every kind of field, plus both nesting shapes.
fn build_form() -> Document {
    let (mut doc, acroform_id, page_id) = form_document();
    add_text_field(&mut doc, acroform_id, page_id, "name");
    add_nested_text(&mut doc, acroform_id, page_id);
    add_checkbox(&mut doc, acroform_id, page_id, "subscribe", "Yes");
    // Not "Yes": filling it must read the on-state from /AP, not assume it.
    add_checkbox(&mut doc, acroform_id, page_id, "agree", "Agreed");
    add_split_checkbox(&mut doc, acroform_id, page_id, "mailing", "Yes");
    add_radio(&mut doc, acroform_id, page_id, "color", &["Red", "Blue"]);
    add_choice(
        &mut doc,
        acroform_id,
        page_id,
        "planet",
        vec![
            Object::string_literal("Mercury"),
            Object::string_literal("Venus"),
            // An export value paired with the label a viewer shows.
            Object::Array(vec![
                Object::string_literal("Earth"),
                Object::string_literal("Earth — third planet"),
            ]),
        ],
    );
    add_signature(&mut doc, acroform_id, page_id, "sig");
    doc
}

// ------------------------------------------------------------------ helpers

/// Save, fill, save and reload, so every assertion reads a real file.
fn fill_and_reload(
    workspace: &Workspace,
    doc: Document,
    data: &[(&str, FieldValue)],
) -> (FillFormReport, Document) {
    let input = workspace.write("form.pdf", doc);
    let mut opened = pdf_core::Document::open(&input).unwrap();

    let data: BTreeMap<String, FieldValue> = data
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect();
    let report = fill_form(&mut opened, &data).unwrap();

    let out = workspace.join("filled.pdf");
    opened.save(&out).unwrap();
    (report, lopdf::Document::load(&out).unwrap())
}

fn acroform(doc: &lopdf::Document) -> &Dictionary {
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .unwrap();
    match doc.get_dictionary(root).unwrap().get(b"AcroForm").unwrap() {
        Object::Reference(id) => doc.get_dictionary(*id).unwrap(),
        Object::Dictionary(form) => form,
        other => panic!("AcroForm is not a dictionary: {other:?}"),
    }
}

fn resolve<'a>(doc: &'a lopdf::Document, object: &'a Object) -> &'a Dictionary {
    match object {
        Object::Reference(id) => doc.get_dictionary(*id).unwrap(),
        Object::Dictionary(dict) => dict,
        other => panic!("expected a dictionary, found {other:?}"),
    }
}

/// The field dictionary with this fully-qualified name, searched the way
/// `fill_form` searches.
fn find_field<'a>(doc: &'a lopdf::Document, qualified: &str) -> &'a Dictionary {
    fn search<'a>(
        doc: &'a lopdf::Document,
        entries: &'a [Object],
        prefix: &str,
        target: &str,
    ) -> Option<&'a Dictionary> {
        for entry in entries {
            let dict = resolve(doc, entry);
            // Widget kids carry no /T; they are covered by their field.
            let Ok(Object::String(partial, _)) = dict.get(b"T") else {
                continue;
            };
            let partial = String::from_utf8_lossy(partial).into_owned();
            let qualified = if prefix.is_empty() {
                partial
            } else {
                format!("{prefix}.{partial}")
            };
            if qualified == target {
                return Some(dict);
            }
            if let Ok(Object::Array(kids)) = dict.get(b"Kids") {
                if let Some(found) = search(doc, kids, &qualified, target) {
                    return Some(found);
                }
            }
        }
        None
    }

    let Object::Array(fields) = acroform(doc).get(b"Fields").unwrap() else {
        panic!("no /Fields array");
    };
    search(doc, fields, "", qualified).unwrap_or_else(|| panic!("no field named {qualified}"))
}

/// The dictionaries that draw a field: itself when merged, plus its widget kids.
fn widget_dicts<'a>(doc: &'a lopdf::Document, field: &'a Dictionary) -> Vec<&'a Dictionary> {
    let mut widgets = Vec::new();
    if field
        .get(b"Subtype")
        .ok()
        .and_then(|subtype| subtype.as_name().ok())
        == Some(b"Widget".as_slice())
    {
        widgets.push(field);
    }
    if let Ok(Object::Array(kids)) = field.get(b"Kids") {
        for kid in kids {
            let dict = resolve(doc, kid);
            if dict.get(b"T").is_err() {
                widgets.push(dict);
            }
        }
    }
    widgets
}

/// Every widget's `/AS` name, in field order.
fn appearance_states(doc: &lopdf::Document, qualified: &str) -> Vec<Vec<u8>> {
    widget_dicts(doc, find_field(doc, qualified))
        .iter()
        .map(|widget| {
            widget
                .get(b"AS")
                .and_then(Object::as_name)
                .unwrap()
                .to_vec()
        })
        .collect()
}

fn field_value(doc: &lopdf::Document, qualified: &str) -> Object {
    find_field(doc, qualified).get(b"V").unwrap().clone()
}

fn need_appearances(doc: &lopdf::Document) -> Option<bool> {
    acroform(doc)
        .get(b"NeedAppearances")
        .ok()
        .and_then(|object| object.as_bool().ok())
}

// -------------------------------------------------------------------- tests

#[test]
fn a_text_field_filled_round_trips_through_save_and_reopen() {
    let workspace = Workspace::new();
    let (report, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[("name", FieldValue::Text("Alice".to_string()))],
    );

    assert_eq!(report.filled, ["name"]);
    assert_eq!(
        field_value(&reloaded, "name"),
        Object::string_literal("Alice")
    );
}

#[test]
fn non_ascii_text_is_written_as_utf16be() {
    let workspace = Workspace::new();
    let (_, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[("name", FieldValue::Text("Amélie".to_string()))],
    );

    let Object::String(bytes, _) = field_value(&reloaded, "name") else {
        panic!("not a string value");
    };
    let mut expected = vec![0xFE, 0xFF];
    for unit in "Amélie".encode_utf16() {
        expected.extend_from_slice(&unit.to_be_bytes());
    }
    assert_eq!(bytes, expected);
}

#[test]
fn nested_field_names_resolve_as_parent_child() {
    let workspace = Workspace::new();
    let (report, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[(
            "contact.email",
            FieldValue::Text("alice@example.com".to_string()),
        )],
    );

    assert_eq!(report.filled, ["contact.email"]);
    assert_eq!(
        field_value(&reloaded, "contact.email"),
        Object::string_literal("alice@example.com")
    );
}

#[test]
fn a_checked_box_shows_its_own_on_state() {
    let workspace = Workspace::new();
    let (report, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[("agree", FieldValue::Checkbox(true))],
    );

    assert_eq!(report.filled, ["agree"]);
    // "Agreed", not an assumed "Yes".
    assert_eq!(
        field_value(&reloaded, "agree"),
        Object::Name(b"Agreed".to_vec())
    );
    assert_eq!(appearance_states(&reloaded, "agree"), [b"Agreed".to_vec()]);
}

#[test]
fn an_unchecked_box_shows_off() {
    let workspace = Workspace::new();
    let (report, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[("subscribe", FieldValue::Checkbox(false))],
    );

    assert_eq!(report.filled, ["subscribe"]);
    assert_eq!(
        field_value(&reloaded, "subscribe"),
        Object::Name(b"Off".to_vec())
    );
    assert_eq!(appearance_states(&reloaded, "subscribe"), [b"Off".to_vec()]);
}

#[test]
fn a_split_checkbox_sets_the_widget_kids_appearance() {
    let workspace = Workspace::new();
    let (report, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[("mailing", FieldValue::Checkbox(true))],
    );

    assert_eq!(report.filled, ["mailing"]);
    assert_eq!(
        field_value(&reloaded, "mailing"),
        Object::Name(b"Yes".to_vec())
    );
    // The field holds the value; the widget kid holds the state.
    assert_eq!(appearance_states(&reloaded, "mailing"), [b"Yes".to_vec()]);
}

#[test]
fn a_checkbox_without_appearance_states_falls_back_to_yes() {
    let workspace = Workspace::new();
    let (mut doc, acroform_id, page_id) = form_document();
    let id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Btn",
        "T" => Object::string_literal("consent"),
        "V" => Object::Name(b"Off".to_vec()),
        "Rect" => rect(72, 620, 20, 20),
    });
    push_reference(&mut doc, acroform_id, b"Fields", id);
    push_reference(&mut doc, page_id, b"Annots", id);

    let (report, reloaded) =
        fill_and_reload(&workspace, doc, &[("consent", FieldValue::Checkbox(true))]);

    assert_eq!(report.filled, ["consent"]);
    assert_eq!(
        field_value(&reloaded, "consent"),
        Object::Name(b"Yes".to_vec())
    );
}

#[test]
fn a_radio_choice_turns_one_widget_on_and_the_rest_off() {
    let workspace = Workspace::new();
    let (report, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[("color", FieldValue::Text("Red".to_string()))],
    );

    assert_eq!(report.filled, ["color"]);
    assert_eq!(
        field_value(&reloaded, "color"),
        Object::Name(b"Red".to_vec())
    );
    assert_eq!(
        appearance_states(&reloaded, "color"),
        [b"Red".to_vec(), b"Off".to_vec()]
    );
}

#[test]
fn a_radio_choice_the_group_does_not_have_is_unsupported() {
    let workspace = Workspace::new();
    let (report, _) = fill_and_reload(
        &workspace,
        build_form(),
        &[("color", FieldValue::Text("Green".to_string()))],
    );

    assert_eq!(report.filled, Vec::<String>::new());
    assert_eq!(report.unsupported.len(), 1);
    let entry = &report.unsupported[0];
    assert!(entry.starts_with("color:"), "got {entry:?}");
    assert!(
        entry.contains("Red") && entry.contains("Blue"),
        "got {entry:?}"
    );
}

#[test]
fn a_boolean_for_a_radio_button_is_unsupported() {
    let workspace = Workspace::new();
    let (report, _) = fill_and_reload(
        &workspace,
        build_form(),
        &[("color", FieldValue::Checkbox(true))],
    );

    assert_eq!(report.unsupported.len(), 1);
    assert!(
        report.unsupported[0].contains("choice name"),
        "got {:?}",
        report.unsupported[0]
    );
}

#[test]
fn a_choice_field_accepts_its_plain_and_paired_export_values() {
    let workspace = Workspace::new();
    // "Earth" is the export value of an /Opt pair, not its display label.
    let (report, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[
            ("planet", FieldValue::Text("Earth".to_string())),
            ("name", FieldValue::Text("Alice".to_string())),
        ],
    );

    assert_eq!(report.filled, ["name", "planet"]);
    assert_eq!(
        field_value(&reloaded, "planet"),
        Object::string_literal("Earth")
    );
    assert_eq!(
        field_value(&reloaded, "name"),
        Object::string_literal("Alice")
    );
}

#[test]
fn a_choice_value_missing_from_opt_is_unsupported() {
    let workspace = Workspace::new();
    let (report, _) = fill_and_reload(
        &workspace,
        build_form(),
        &[("planet", FieldValue::Text("Pluto".to_string()))],
    );

    assert_eq!(report.unsupported.len(), 1);
    let entry = &report.unsupported[0];
    assert!(entry.starts_with("planet:"), "got {entry:?}");
    assert!(
        entry.contains("Mercury") && entry.contains("Earth"),
        "the reason should list the options, got {entry:?}"
    );
}

#[test]
fn signature_fields_cannot_be_filled() {
    let workspace = Workspace::new();
    let (report, _) = fill_and_reload(
        &workspace,
        build_form(),
        &[("sig", FieldValue::Text("Alice".to_string()))],
    );

    assert_eq!(
        report.unsupported,
        ["sig: signature fields cannot be filled"]
    );
}

#[test]
fn unknown_names_land_in_not_found() {
    let workspace = Workspace::new();
    let (report, _) = fill_and_reload(
        &workspace,
        build_form(),
        &[
            ("name", FieldValue::Text("Alice".to_string())),
            ("middle_name", FieldValue::Text("Q".to_string())),
        ],
    );

    assert_eq!(report.filled, ["name"]);
    assert_eq!(report.not_found, ["middle_name"]);
}

#[test]
fn values_of_the_wrong_kind_are_unsupported() {
    let workspace = Workspace::new();
    let (report, _) = fill_and_reload(
        &workspace,
        build_form(),
        &[
            ("name", FieldValue::Checkbox(true)),
            ("planet", FieldValue::Checkbox(false)),
        ],
    );

    assert_eq!(report.unsupported.len(), 2);
    assert!(
        report.unsupported[0].starts_with("name:"),
        "got {:?}",
        report.unsupported
    );
    assert!(
        report.unsupported[1].starts_with("planet:"),
        "got {:?}",
        report.unsupported
    );
}

#[test]
fn filling_sets_need_appearances_so_viewers_rebuild_appearances() {
    let workspace = Workspace::new();
    let (_, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[("name", FieldValue::Text("Alice".to_string()))],
    );

    assert_eq!(need_appearances(&reloaded), Some(true));
}

#[test]
fn nothing_filled_leaves_need_appearances_alone() {
    let workspace = Workspace::new();
    let (_, reloaded) = fill_and_reload(
        &workspace,
        build_form(),
        &[("no_such_field", FieldValue::Text("x".to_string()))],
    );

    assert_eq!(need_appearances(&reloaded), Some(false));
}

#[test]
fn a_document_without_a_form_reports_every_name_not_found() {
    let workspace = Workspace::new();
    // The shared generator writes no AcroForm.
    let doc = common::build(1, "Page", A4, Attributes::OnPage);

    let (report, _) = fill_and_reload(
        &workspace,
        doc,
        &[("name", FieldValue::Text("Alice".to_string()))],
    );

    assert_eq!(report.filled, Vec::<String>::new());
    assert_eq!(report.not_found, ["name"]);
}

#[test]
fn form_data_maps_booleans_to_checkboxes_and_strings_to_text() {
    let data = pdf_core::parse_form_data(r#"{"agree": true, "name": "Ada"}"#).unwrap();
    assert_eq!(data["agree"], pdf_core::FieldValue::Checkbox(true));
    assert_eq!(data["name"], pdf_core::FieldValue::Text("Ada".into()));
}

#[test]
fn form_data_names_the_field_with_a_wrong_value() {
    let error = pdf_core::parse_form_data(r#"{"age": 42}"#).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("\"age\""), "{message}");
    assert!(message.contains("a number"), "{message}");
}

#[test]
fn form_data_must_be_an_object_and_valid_json() {
    assert!(pdf_core::parse_form_data("[1, 2]").is_err());
    assert!(pdf_core::parse_form_data("{not json").is_err());
}
