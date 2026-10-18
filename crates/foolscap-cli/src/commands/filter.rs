use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use pdf_core::{Document, PageRange};

use crate::output::{count, guard_output};

/// The per-pixel transforms on offer.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Kind {
    /// Flip every sample. For dark-mode reading of scans.
    Invert,
    /// Drop the colour, keeping the luminance.
    Grayscale,
}

impl From<Kind> for pdf_core::Filter {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Invert => pdf_core::Filter::Invert,
            Kind::Grayscale => pdf_core::Filter::Grayscale,
        }
    }
}

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to filter.
    input: PathBuf,

    /// Pages whose images to filter, such as 1-3,7 or all. Defaults to every page.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Which filter to apply. Only raster images change; vector text and
    /// line art are left alone.
    #[arg(long, value_enum)]
    filter: Kind,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let range = PageRange::parse(&args.pages)?;
    let mut doc = Document::open_with(&args.input, password)?;

    let mut report_progress = |progress: pdf_core::Progress| {
        tracing::info!("{}", progress.message);
    };

    let report = pdf_core::filter_with_progress(
        &mut doc,
        &range,
        args.filter.into(),
        Some(&mut report_progress),
    )?;
    doc.save(&args.output)?;

    println!(
        "{} filtered, {} left alone",
        count(report.images_filtered, "image"),
        count(report.images_skipped(), "image"),
    );
    println!("{}", args.output.display());
    Ok(())
}
