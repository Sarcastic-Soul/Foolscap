//! `from-images`, `to-images`, `from-office` and `to-office`. Only built with
//! the `convert` feature.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use pdf_core::convert::{
    images_to_pdf_with_progress, office_to_pdf, pdf_to_office, Fit, ImagesToPdfOptions,
    OfficeFormat, PageSize,
};

use crate::output::guard_output;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Size {
    A4,
    Letter,
    Legal,
    /// One page per image, sized to the image itself.
    Fit,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FitMode {
    /// Fit the whole image on the page, leaving margins.
    Contain,
    /// Fill the page, cropping the overflow.
    Cover,
    /// Fill the page exactly, distorting the image.
    Stretch,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Format {
    Docx,
    Odt,
    Rtf,
}

impl From<Format> for OfficeFormat {
    fn from(format: Format) -> Self {
        match format {
            Format::Docx => OfficeFormat::Docx,
            Format::Odt => OfficeFormat::Odt,
            Format::Rtf => OfficeFormat::Rtf,
        }
    }
}

#[derive(Debug, Parser)]
pub struct FromImagesArgs {
    /// Images to bind into a document, in order.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Page size for every page.
    #[arg(long, value_enum, default_value_t = Size::A4)]
    page_size: Size,

    /// How each image is placed on its page.
    #[arg(long, value_enum, default_value_t = FitMode::Contain)]
    fit: FitMode,

    /// Blank border in points, on all four sides.
    #[arg(long, default_value_t = 0.0)]
    margin: f32,

    /// Resolution used to turn pixels into page size. Only affects --page-size fit.
    #[arg(long, default_value_t = 300.0)]
    dpi: f32,

    /// Where to write the document.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Debug, Parser)]
pub struct FromOfficeArgs {
    /// The document to convert. Anything LibreOffice can open.
    input: PathBuf,

    /// Where to write the PDF.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Debug, Parser)]
pub struct ToOfficeArgs {
    /// The PDF to convert.
    input: PathBuf,

    /// Format to produce. No short flag: -f is the global --force.
    #[arg(long, value_enum, default_value_t = Format::Docx)]
    format: Format,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn from_images(args: FromImagesArgs, force: bool) -> Result<()> {
    if !(args.dpi.is_finite() && args.dpi > 0.0) {
        anyhow::bail!("--dpi must be a positive number, got {}", args.dpi);
    }

    guard_output(&args.output, force)?;

    let options = ImagesToPdfOptions {
        page_size: match args.page_size {
            Size::A4 => PageSize::A4,
            Size::Letter => PageSize::Letter,
            Size::Legal => PageSize::Legal,
            Size::Fit => PageSize::FitImage,
        },
        fit: match args.fit {
            FitMode::Contain => Fit::Contain,
            FitMode::Cover => Fit::Cover,
            FitMode::Stretch => Fit::Stretch,
        },
        margin: args.margin,
        dpi: args.dpi,
    };

    let mut report = |progress: pdf_core::Progress| tracing::info!("{}", progress.message);
    images_to_pdf_with_progress(&args.inputs, &args.output, options, Some(&mut report))?;

    println!("{}", args.output.display());
    Ok(())
}

pub fn from_office(args: FromOfficeArgs, force: bool) -> Result<()> {
    guard_output(&args.output, force)?;

    // LibreOffice chooses the output name itself, so convert into a scratch
    // directory beside the target and then move the result into place.
    let scratch = scratch_beside(&args.output)?;
    let produced = office_to_pdf(&args.input, scratch.path())?;
    std::fs::rename(&produced, &args.output)
        .or_else(|_| std::fs::copy(&produced, &args.output).map(|_| ()))?;

    println!("{}", args.output.display());
    Ok(())
}

pub fn to_office(args: ToOfficeArgs, force: bool) -> Result<()> {
    guard_output(&args.output, force)?;

    // Worth saying every time, not just in the documentation: a PDF records
    // marks on a page, and the structure a word processor needs has to be
    // guessed back from them.
    eprintln!("warning: converting a PDF to an editable format is approximate;");
    eprintln!("         expect layout, fonts and structure to differ from the original.");

    let scratch = scratch_beside(&args.output)?;
    let produced = pdf_to_office(&args.input, args.format.into(), scratch.path())?;
    std::fs::rename(&produced, &args.output)
        .or_else(|_| std::fs::copy(&produced, &args.output).map(|_| ()))?;

    println!("{}", args.output.display());
    Ok(())
}

/// A scratch directory next to the eventual output, so that moving the result
/// into place stays on one filesystem.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn scratch_beside(output: &std::path::Path) -> Result<Scratch> {
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
    let base = match parent {
        Some(parent) => {
            std::fs::create_dir_all(parent)?;
            parent.to_path_buf()
        }
        None => PathBuf::from("."),
    };

    let path = base.join(format!(".foolscap-{}", std::process::id()));
    std::fs::create_dir_all(&path)?;

    Ok(Scratch { path })
}
