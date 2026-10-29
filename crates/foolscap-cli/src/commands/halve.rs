use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use pdf_core::{halve, Document, HalveDirection, HalveOptions};

use crate::output::guard_output;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The document to halve.
    input: PathBuf,

    /// Which way to cut: vertical (left/right) or horizontal (top/bottom).
    #[arg(long, default_value = "vertical")]
    direction: String,

    /// Margin to drop at the cut line, in points, for a gutter.
    #[arg(long, default_value_t = 0.0)]
    gap: f64,

    /// Emit the halves in swapped order.
    #[arg(long)]
    swap: bool,

    /// Where to write the halved document.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    // Parsed before anything is read, so a bad value fails before work starts.
    let direction = match args.direction.trim().to_ascii_lowercase().as_str() {
        "vertical" => HalveDirection::Vertical,
        "horizontal" => HalveDirection::Horizontal,
        other => bail!("--direction must be vertical or horizontal, not {other:?}"),
    };

    guard_output(&args.output, force)?;

    let mut doc = Document::open_with(&args.input, password)?;
    let before = doc.page_count();

    halve(
        &mut doc,
        &HalveOptions {
            direction,
            gap: args.gap,
            swap: args.swap,
        },
    )?;

    doc.save(&args.output)?;

    println!(
        "cut {} into {} {}",
        crate::output::pages(before),
        crate::output::pages(doc.page_count()),
        match direction {
            HalveDirection::Vertical => "left/right halves",
            HalveDirection::Horizontal => "top/bottom halves",
        }
    );
    println!("{}", args.output.display());
    Ok(())
}
