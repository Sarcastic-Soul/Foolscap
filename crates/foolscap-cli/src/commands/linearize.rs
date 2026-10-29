use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use pdf_core::{is_linearized, ops::linearize::write_linearized, Document};

use crate::output::guard_output;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The document to linearize.
    input: PathBuf,

    /// Where to write the linearized document.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let doc = Document::open_with(&args.input, password)?;
    // `write_linearized` keeps the two-section layout; `Document::save` would
    // flatten it back to a single cross-reference section.
    write_linearized(&doc, &args.output)?;

    // Confirm the file on disk carries the dictionary, not just the plan.
    let written = Document::open(&args.output)?;
    if !is_linearized(&written) {
        bail!("linearization completed but the document carries no linearization dictionary");
    }

    println!("{}", args.output.display());
    Ok(())
}
