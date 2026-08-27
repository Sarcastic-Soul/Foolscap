use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;

use crate::output::{guard_output, pages};

#[derive(Debug, Parser)]
pub struct Args {
    /// Documents to concatenate, in the order they should appear.
    #[arg(required = true, num_args = 2..)]
    inputs: Vec<PathBuf>,

    /// Where to write the merged document.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool) -> Result<()> {
    if args.inputs.len() < 2 {
        bail!("merge needs at least two input documents");
    }

    guard_output(&args.output, force)?;

    let mut report = |progress: pdf_core::Progress| {
        tracing::info!("{}", progress.message);
    };

    pdf_core::merge_with_progress(&args.inputs, &args.output, Some(&mut report))?;

    let merged = pdf_core::Document::open(&args.output)?;
    println!(
        "{} -> {} ({})",
        args.inputs.len(),
        args.output.display(),
        pages(merged.page_count())
    );

    Ok(())
}
