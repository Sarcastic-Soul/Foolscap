use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::{Document, PageRange};

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to crop.
    input: PathBuf,

    /// Pages to crop, such as 1-3,7 or all. Defaults to every page.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Crop rectangle as x,y,w,h in PDF points, e.g. "72,72,468,648".
    /// Clamped to the page's media box.
    #[arg(short, long, conflicts_with_all = ["reset", "page_size", "mediabox"])]
    rect: Option<String>,

    /// Show the full media box again by removing the crop box.
    #[arg(long, conflicts_with_all = ["rect", "page_size", "mediabox"])]
    reset: bool,

    /// Set the media box directly as x,y,w,h in PDF points.
    #[arg(long, value_name = "X,Y,W,H", conflicts_with_all = ["rect", "reset", "page_size"])]
    mediabox: Option<String>,

    /// Scale pages to a stock size: a4, letter, legal, or WxH in points.
    /// Scales the drawn content to fit; see --fit.
    #[arg(long, value_name = "SIZE", conflicts_with_all = ["rect", "reset", "mediabox"])]
    page_size: Option<String>,

    /// How content fits the new size: contain, cover, or stretch.
    #[arg(long, default_value = "contain")]
    fit: String,

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

    if args.reset {
        pdf_core::reset_crop(&mut doc, &range)?;
        doc.save(&args.output)?;
        println!("crop removed");
    } else if let Some(size) = args.page_size {
        let size = pdf_core::PageSize::parse(&size)?;
        let fit = pdf_core::Fit::parse(&args.fit)?;
        pdf_core::ops::crop::scale_to_with_progress(&mut doc, &range, size, fit, progress)?;
        doc.save(&args.output)?;
        println!("scaled to {size:?} ({})", args.fit);
    } else if let Some(media) = args.mediabox {
        let rect = pdf_core::Rect::parse_xywh(&media)
            .map_err(|error| anyhow::anyhow!("invalid --mediabox: {error}"))?;
        pdf_core::ops::crop::set_mediabox_with_progress(&mut doc, &range, rect, progress)?;
        doc.save(&args.output)?;
        println!("media box set");
    } else if let Some(rect) = args.rect {
        let rect = pdf_core::Rect::parse_xywh(&rect)
            .map_err(|error| anyhow::anyhow!("invalid --rect: {error}"))?;
        pdf_core::ops::crop::crop_with_progress(&mut doc, &range, rect, progress)?;
        doc.save(&args.output)?;
        println!("cropped");
    } else {
        anyhow::bail!(
            "pass one of --rect X,Y,W,H, --mediabox X,Y,W,H, --page-size SIZE, or --reset"
        );
    }

    println!("{}", args.output.display());
    Ok(())
}
