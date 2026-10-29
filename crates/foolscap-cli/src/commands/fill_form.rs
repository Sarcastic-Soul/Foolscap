//! `foolscap fill-form` — write JSON-supplied values into AcroForm fields.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
    let data = pdf_core::parse_form_data(&raw).with_context(|| format!("{}", path.display()))?;
    Ok(data)
}
