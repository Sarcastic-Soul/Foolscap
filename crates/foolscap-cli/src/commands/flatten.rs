//! `foolscap flatten` — bake form fields and annotations into page content.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::ops::{flatten_with_progress, FlattenOptions};
use pdf_core::{Document, PageRange};

use crate::output::{count, guard_output};

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to flatten.
    input: PathBuf,

    /// Only flatten widget annotations (form fields). Links and markup
    /// annotations are left intact.
    #[arg(long)]
    forms_only: bool,

    /// Only flatten non-widget annotations (links, markup, etc.). Widget
    /// annotations (form fields) are left intact.
    #[arg(long)]
    annots_only: bool,

    /// Pages to flatten, such as 1-3,7 or all.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    if args.forms_only && args.annots_only {
        anyhow::bail!("--forms-only and --annots-only are mutually exclusive");
    }

    guard_output(&args.output, force)?;

    let pages = PageRange::parse(&args.pages)?;

    let options = FlattenOptions {
        forms_only: args.forms_only,
        annots_only: args.annots_only,
    };

    let mut report_progress = |progress: pdf_core::Progress| {
        tracing::info!("{}", progress.message);
    };

    let mut doc = Document::open_with(&args.input, password)?;
    let report = flatten_with_progress(&mut doc, &pages, options, Some(&mut report_progress))?;

    doc.save(&args.output)?;

    println!("{} flattened", count(report.pages_flattened, "page"));
    if report.widgets_flattened > 0 {
        println!("{} baked in", count(report.widgets_flattened, "form field"));
    }
    if report.annots_flattened > 0 {
        println!("{} baked in", count(report.annots_flattened, "annotation"));
    }
    if report.acroform_removed {
        println!("AcroForm dictionary removed");
    }
    println!("{}", args.output.display());

    Ok(())
}
