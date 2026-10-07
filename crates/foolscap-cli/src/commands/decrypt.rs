use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;
use pdf_core::Document;

use crate::output::{guard_output, pages};

#[derive(Debug, Parser)]
pub struct Args {
    /// The encrypted document. Give its password with --password, unless it
    /// opens without one.
    input: PathBuf,

    /// Where to write the decrypted copy.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let mut doc = Document::open_with(&args.input, password)?;

    if !doc.was_encrypted() {
        bail!(
            "{} is not encrypted; there is nothing to decrypt",
            args.input.display()
        );
    }

    doc.save(&args.output)?;

    println!(
        "{} -> {} ({}, no longer encrypted)",
        args.input.display(),
        args.output.display(),
        pages(doc.page_count())
    );

    Ok(())
}
