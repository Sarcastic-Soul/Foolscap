//! `foolscap fill-form` — write JSON-supplied values into AcroForm fields.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use pdf_core::{fill_form, Document, FieldValue};

use crate::output::{count, guard_output};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The form-bearing document to fill.
    input: PathBuf,

    /// JSON file of `{"field name": value}` pairs. Booleans fill checkboxes;
    /// strings fill text and choice fields.
    #[arg(long, value_name = "FILE")]
    data: PathBuf,

    /// Where to write the filled document.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let data = read_data(&args.data)?;

    let mut doc = Document::open_with(&args.input, password)?;
    let report = fill_form(&mut doc, &data)?;
    doc.save(&args.output)?;

    println!("{} filled", count(report.filled.len(), "field"));
    for name in &report.not_found {
        println!("not found: {name}");
    }
    for entry in &report.unsupported {
        println!("unsupported: {entry}");
    }
    println!("{}", args.output.display());

    Ok(())
}

/// Read the JSON file, rejecting values that are neither strings nor booleans
/// by naming the field that carries one — a number in an untagged enum would
/// otherwise fail with a generic "data did not match any variant".
fn read_data(path: &Path) -> Result<BTreeMap<String, FieldValue>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;

    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;

    let serde_json::Value::Object(map) = value else {
        bail!(
            "{} must hold a JSON object of {{\"field name\": value}} pairs",
            path.display()
        );
    };

    let mut data = BTreeMap::new();
    for (name, value) in map {
        let value = match value {
            serde_json::Value::Bool(on) => FieldValue::Checkbox(on),
            serde_json::Value::String(text) => FieldValue::Text(text),
            other => bail!(
                "field \"{name}\": expected a string or a boolean, found {}",
                json_kind(&other)
            ),
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
        // The remaining kinds are the ones already accepted above.
        serde_json::Value::Bool(_) | serde_json::Value::String(_) => "a valid value",
    }
}
