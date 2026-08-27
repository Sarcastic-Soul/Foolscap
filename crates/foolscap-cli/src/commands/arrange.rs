use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::{Document, PageRange};

use crate::output::{guard_output, pages};

#[derive(Debug, Parser)]
pub struct DeleteArgs {
    /// The document to trim.
    input: PathBuf,

    /// Pages to remove, such as 2,4 or 5-.
    #[arg(short, long)]
    pages: String,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Debug, Parser)]
pub struct ReorderArgs {
    /// The document to reorder.
    input: PathBuf,

    /// The new page order, such as 3,1-2. Pages left out are dropped.
    #[arg(short = 'r', long)]
    order: String,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn delete(args: DeleteArgs, force: bool) -> Result<()> {
    guard_output(&args.output, force)?;

    let doc = Document::open(&args.input)?;
    let before = doc.page_count();

    let mut trimmed = pdf_core::delete(doc, &PageRange::parse(&args.pages)?)?;
    let after = trimmed.page_count();
    trimmed.save(&args.output)?;

    println!("{} -> {}", pages(before), pages(after));
    println!("{}", args.output.display());
    Ok(())
}

pub fn reorder(args: ReorderArgs, force: bool) -> Result<()> {
    guard_output(&args.output, force)?;

    let doc = Document::open(&args.input)?;
    let order = PageRange::parse(&args.order)?.resolve(doc.page_count())?;

    let mut arranged = pdf_core::arrange(doc, &order)?;
    arranged.save(&args.output)?;

    println!("{}", args.output.display());
    Ok(())
}
