use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::{Document, PageRange};

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to redact.
    input: PathBuf,

    /// Pages to redact, such as 1-3,7 or all. Defaults to every page.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Rectangle to cover as x,y,w,h in PDF points, e.g. "72,600,200,24".
    /// An opaque box is drawn and the page's text runs are stripped so the
    /// covered words leave no searchable trace.
    #[arg(short, long, conflicts_with = "find")]
    rect: Option<String>,

    /// Redact every text run containing this string instead of a rectangle.
    /// Matching runs are removed wherever they sit; no box is drawn.
    #[arg(long, conflicts_with = "rect")]
    find: Option<String>,

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
    let progress = Some(&mut report_progress as &mut dyn FnMut(pdf_core::Progress));

    if let Some(rect) = args.rect {
        let rect = pdf_core::Rect::parse_xywh(&rect)
            .map_err(|error| anyhow::anyhow!("invalid --rect: {error}"))?;
        let report = pdf_core::ops::redact::redact_with_progress(&mut doc, &range, rect, progress)?;
        doc.save(&args.output)?;
        println!(
            "redacted {} on {}",
            crate::output::count(report.boxes_drawn, "area"),
            crate::output::pages(report.pages_redacted),
        );
    } else if let Some(needle) = args.find {
        let report =
            pdf_core::ops::redact::redact_find_with_progress(&mut doc, &range, &needle, progress)?;
        doc.save(&args.output)?;
        println!(
            "removed {} containing {needle:?} on {}",
            crate::output::count(report.runs_removed, "run"),
            crate::output::pages(report.pages_redacted),
        );
    } else {
        anyhow::bail!("pass one of --rect X,Y,W,H or --find TEXT");
    }

    println!("{}", args.output.display());
    Ok(())
}
