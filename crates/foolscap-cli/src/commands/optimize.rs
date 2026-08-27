use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::{Document, OptimizeLevel};

use crate::output::{count, guard_output, human_bytes};

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to shrink.
    input: PathBuf,

    /// Also deduplicate identical streams and recompress. Still lossless.
    #[arg(short, long)]
    aggressive: bool,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool) -> Result<()> {
    guard_output(&args.output, force)?;

    let level = if args.aggressive {
        OptimizeLevel::Aggressive
    } else {
        OptimizeLevel::Safe
    };

    let mut doc = Document::open(&args.input)?;
    let report = pdf_core::optimize(&mut doc, level)?;
    doc.save(&args.output)?;

    println!(
        "{} -> {} ({:.1}% smaller, {} removed)",
        human_bytes(report.bytes_before),
        human_bytes(report.bytes_after),
        report.ratio_saved() * 100.0,
        count(report.objects_removed, "object"),
    );
    println!("{}", args.output.display());

    Ok(())
}
