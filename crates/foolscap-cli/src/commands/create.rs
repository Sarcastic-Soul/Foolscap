use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use pdf_core::{create_blank, PageSize};

use crate::output::{guard_output, pages};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// How many pages to create.
    #[arg(long, default_value_t = 1)]
    pages: usize,

    /// Page size: a4, letter, legal, or WxH in points (e.g. 300x600).
    #[arg(long, default_value = "a4")]
    size: String,

    /// Where to write the blank document.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool) -> Result<()> {
    // Parsed before anything is written, so a bad value fails before work
    // starts. The syntax is the one `crop --page-size` accepts.
    let size = PageSize::parse(&args.size)?;
    if args.pages == 0 {
        bail!("--pages must be at least 1");
    }

    guard_output(&args.output, force)?;

    let mut doc = create_blank(args.pages, size)?;
    doc.save(&args.output)?;

    println!("created {} at {}", pages(args.pages), size_desc(&size));
    println!("{}", args.output.display());
    Ok(())
}

fn size_desc(size: &PageSize) -> String {
    match size.points() {
        Some((width, height)) => format!("{width:.0}x{height:.0} points"),
        None => "fit to content".to_string(),
    }
}
