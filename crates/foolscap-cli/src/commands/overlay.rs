//! `foolscap overlay` — draw one PDF on top of another.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to draw on top of.
    base: PathBuf,

    /// The document to draw over the base.
    overlay: PathBuf,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    crate::output::guard_output(&args.output, force)?;
    pdf_core::overlay(&args.base, &args.overlay, &args.output, password)?;
    println!("{}", args.output.display());
    Ok(())
}
