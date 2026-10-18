//! `foolscap to-images`. Needs both `convert` and `render`.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use pdf_core::convert::{
    pdf_to_images_with_progress, pdf_to_zip_with_progress, ImageFormat, PdfToImagesOptions,
};
use pdf_core::PageRange;

use crate::output::guard_output;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Format {
    Png,
    Jpeg,
}

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to rasterise.
    input: PathBuf,

    /// Pages to convert, such as 1-3,7 or all.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Resolution in dots per inch.
    #[arg(short, long, default_value_t = 150.0)]
    dpi: f32,

    /// Output image format.
    #[arg(long, value_enum, default_value_t = Format::Png)]
    format: Format,

    /// JPEG quality, 1 to 100. Ignored for PNG.
    #[arg(long, default_value_t = 85)]
    quality: u8,

    /// Directory to write the images into.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    if !(args.dpi.is_finite() && args.dpi > 0.0) {
        anyhow::bail!("--dpi must be a positive number, got {}", args.dpi);
    }
    if args.quality == 0 || args.quality > 100 {
        anyhow::bail!("--quality must be between 1 and 100, got {}", args.quality);
    }

    let options = PdfToImagesOptions {
        pages: PageRange::parse(&args.pages)?,
        dpi: args.dpi,
        format: match args.format {
            Format::Png => ImageFormat::Png,
            Format::Jpeg => ImageFormat::Jpeg,
        },
        quality: args.quality,
        password: password.map(str::to_string),
    };

    for path in pdf_core::convert::plan(&args.input, &args.output, &options)? {
        guard_output(&path, force)?;
    }

    let mut report = |progress: pdf_core::Progress| tracing::info!("{}", progress.message);
    let written =
        pdf_to_images_with_progress(&args.input, &args.output, &options, Some(&mut report))?;

    for path in &written {
        println!("{}", path.display());
    }

    Ok(())
}

#[derive(Debug, Parser)]
pub struct ZipArgs {
    /// The document to rasterise.
    input: PathBuf,

    /// Pages to convert, such as 1-3,7 or all.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Resolution in dots per inch.
    #[arg(short, long, default_value_t = 150.0)]
    dpi: f32,

    /// Image format inside the archive.
    #[arg(long, value_enum, default_value_t = Format::Png)]
    format: Format,

    /// JPEG quality, 1 to 100. Ignored for PNG.
    #[arg(long, default_value_t = 85)]
    quality: u8,

    /// Where to write the zip archive.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn zip(args: ZipArgs, force: bool, password: Option<&str>) -> Result<()> {
    if !(args.dpi.is_finite() && args.dpi > 0.0) {
        anyhow::bail!("--dpi must be a positive number, got {}", args.dpi);
    }
    if args.quality == 0 || args.quality > 100 {
        anyhow::bail!("--quality must be between 1 and 100, got {}", args.quality);
    }

    guard_output(&args.output, force)?;

    let options = PdfToImagesOptions {
        pages: PageRange::parse(&args.pages)?,
        dpi: args.dpi,
        format: match args.format {
            Format::Png => ImageFormat::Png,
            Format::Jpeg => ImageFormat::Jpeg,
        },
        quality: args.quality,
        password: password.map(str::to_string),
    };

    let mut report = |progress: pdf_core::Progress| tracing::info!("{}", progress.message);
    let names = pdf_to_zip_with_progress(&args.input, &args.output, &options, Some(&mut report))?;

    println!("{} ({} images)", args.output.display(), names.len());
    Ok(())
}
