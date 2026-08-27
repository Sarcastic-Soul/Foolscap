use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use pdf_core::{CompressLevel, Document};

use crate::output::{count, guard_output, human_bytes};

/// The level names users already know from other PDF tools.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Level {
    /// 72 dpi. For reading on a screen.
    Screen,
    /// 150 dpi. Sharp on a display, adequate on paper.
    Ebook,
    /// 300 dpi. Print quality, so only oversampled images shrink.
    Print,
}

impl From<Level> for CompressLevel {
    fn from(level: Level) -> Self {
        match level {
            Level::Screen => CompressLevel::Screen,
            Level::Ebook => CompressLevel::Ebook,
            Level::Print => CompressLevel::Print,
        }
    }
}

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to compress.
    input: PathBuf,

    /// How hard to squeeze. This pass is lossy; use `optimize` to stay lossless.
    #[arg(short, long, value_enum, default_value_t = Level::Ebook)]
    level: Level,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool) -> Result<()> {
    guard_output(&args.output, force)?;

    let mut report_progress = |progress: pdf_core::Progress| {
        tracing::info!("{}", progress.message);
    };

    let mut doc = Document::open(&args.input)?;
    let report =
        pdf_core::compress_with_progress(&mut doc, args.level.into(), Some(&mut report_progress))?;
    doc.save(&args.output)?;

    println!(
        "{} -> {} ({:.1}% smaller)",
        human_bytes(report.bytes_before),
        human_bytes(report.bytes_after),
        report.ratio_saved() * 100.0,
    );
    println!(
        "{} recompressed, {} left alone",
        count(report.images_recompressed, "image"),
        count(report.images_skipped(), "image"),
    );

    // Say why, so a document that barely shrank is explicable.
    let mut reasons: Vec<_> = report.skipped.iter().collect();
    reasons.sort_by_key(|(reason, _)| format!("{reason:?}"));
    for (reason, number) in reasons {
        println!("  {number} {}", describe(*reason));
    }

    println!("{}", args.output.display());
    Ok(())
}

fn describe(reason: pdf_core::SkipReason) -> &'static str {
    use pdf_core::SkipReason::*;

    match reason {
        Mask => "used as a mask",
        UnsupportedFilter => "in a format this build cannot decode",
        UnsupportedColorSpace => "in a colour space this build cannot decode",
        NeverDrawn => "never drawn on any page",
        AlreadySmall => "already at or below the target resolution",
        NoSaving => "would not have got any smaller",
        Tiny => "too small to be worth touching",
    }
}
