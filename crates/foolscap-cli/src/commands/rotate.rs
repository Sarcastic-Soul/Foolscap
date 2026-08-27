use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::{Document, PageRange};

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to rotate.
    input: PathBuf,

    /// Pages to rotate, such as 1-3,7 or all. Defaults to every page.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Degrees to rotate by, clockwise. Must be a multiple of 90.
    #[arg(short, long, allow_negative_numbers = true)]
    degrees: i32,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool) -> Result<()> {
    guard_output(&args.output, force)?;

    let range = PageRange::parse(&args.pages)?;
    let mut doc = Document::open(&args.input)?;

    pdf_core::rotate(&mut doc, &range, args.degrees)?;
    doc.save(&args.output)?;

    println!("{}", args.output.display());
    Ok(())
}
