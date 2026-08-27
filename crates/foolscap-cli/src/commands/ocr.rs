//! `foolscap ocr` and `foolscap extract-text`. Only built with the `ocr`
//! feature; extraction additionally needs `render`, which `ocr` implies.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::ocr::{self, OcrOptions, DEFAULT_DPI};
use pdf_core::render::PageRenderer;
use pdf_core::PageRange;

use crate::output::{count, guard_output};

#[derive(Debug, Parser)]
pub struct OcrArgs {
    /// The document to make searchable.
    input: PathBuf,

    /// Tesseract language code, or several joined with +, such as eng+deu.
    #[arg(short, long, default_value = "eng")]
    lang: String,

    /// Resolution to recognise at. Tesseract is trained around 300.
    #[arg(short, long, default_value_t = DEFAULT_DPI)]
    dpi: f32,

    /// Recognise every page, including ones that already have text.
    #[arg(long)]
    redo: bool,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Debug, Parser)]
pub struct ExtractTextArgs {
    /// The document to read.
    input: PathBuf,

    /// Pages to read, such as 1-3,7 or all.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Where to write the text. Omit to write to standard output.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

pub fn run(args: OcrArgs, force: bool) -> Result<()> {
    if !(args.dpi.is_finite() && args.dpi > 0.0) {
        anyhow::bail!("--dpi must be a positive number, got {}", args.dpi);
    }

    guard_output(&args.output, force)?;

    let options = OcrOptions {
        language: args.lang,
        dpi: args.dpi,
        skip_pages_with_text: !args.redo,
    };

    let mut report_progress = |progress: pdf_core::Progress| {
        tracing::info!("{}", progress.message);
    };

    let report = ocr::ocr_with_progress(
        &args.input,
        &args.output,
        &options,
        Some(&mut report_progress),
    )?;

    println!("{} recognised", count(report.pages_recognised, "page"));
    if report.pages_already_text > 0 {
        println!(
            "{} already had text",
            count(report.pages_already_text, "page")
        );
    }
    if report.pages_without_text > 0 {
        println!(
            "{} had nothing to recognise",
            count(report.pages_without_text, "page")
        );
    }
    println!("{}", args.output.display());

    Ok(())
}

pub fn extract_text(args: ExtractTextArgs, force: bool) -> Result<()> {
    if let Some(output) = &args.output {
        guard_output(output, force)?;
    }

    let renderer = PageRenderer::open(&args.input)?;
    let selected = PageRange::parse(&args.pages)?.resolve(renderer.page_count())?;

    let mut text = String::new();
    for page in selected {
        text.push_str(&renderer.page_text(page)?);
        // A form feed is the conventional page separator in extracted text, and
        // keeps the boundary recoverable by whatever reads this next.
        text.push('\u{c}');
    }

    match &args.output {
        Some(path) => {
            std::fs::write(path, text)?;
            println!("{}", path.display());
        }
        None => print!("{text}"),
    }

    Ok(())
}
