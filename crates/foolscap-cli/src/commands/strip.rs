//! `foolscap strip` — remove identifying metadata before sharing.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::Document;

use crate::output::{count, guard_output};

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to clean.
    input: PathBuf,

    /// Where to write the cleaned copy.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let mut doc = Document::open_with(&args.input, password)?;
    let report = pdf_core::strip(&mut doc)?;
    doc.save(&args.output)?;

    println!(
        "{} -> {} (removed {}, {}{})",
        args.input.display(),
        args.output.display(),
        count(report.info_keys_removed, "info entry").replace("entrys", "entries"),
        count(report.xmp_removed, "XMP stream"),
        if report.id_rotated {
            ", new document ID"
        } else {
            ""
        },
    );

    Ok(())
}
