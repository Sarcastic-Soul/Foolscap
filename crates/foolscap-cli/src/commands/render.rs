//! `foolscap render` and `foolscap thumbnail`. Only built with the `render`
//! feature.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use image::{ExtendedColorType, ImageEncoder};
use pdf_core::render::{PageRenderer, Scale, DEFAULT_DPI};
use pdf_core::{PageRange, RenderedPage};

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct RenderArgs {
    /// The document to rasterise.
    input: PathBuf,

    /// Pages to render, such as 1-3,7 or all.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Resolution in dots per inch.
    #[arg(short, long, default_value_t = DEFAULT_DPI)]
    dpi: f32,

    /// Directory to write the PNGs into.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Debug, Parser)]
pub struct ThumbnailArgs {
    /// The document to take a thumbnail of.
    input: PathBuf,

    /// Which page, one-indexed.
    #[arg(short, long, default_value_t = 1)]
    page: usize,

    /// Longest edge of the result, in pixels.
    #[arg(short, long, default_value_t = 256)]
    size: u32,

    /// Where to write the PNG.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn render(args: RenderArgs, force: bool) -> Result<()> {
    if !(args.dpi.is_finite() && args.dpi > 0.0) {
        anyhow::bail!("--dpi must be a positive number, got {}", args.dpi);
    }

    let renderer = PageRenderer::open(&args.input)?;
    let selected = PageRange::parse(&args.pages)?.resolve(renderer.page_count())?;

    let stem = args
        .input
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page".to_string());
    let width = renderer.page_count().max(1).to_string().len();

    // Names first, so a collision is reported before anything is written.
    let paths: Vec<PathBuf> = selected
        .iter()
        .map(|page| {
            args.output
                .join(format!("{stem}-{:0width$}.png", page + 1, width = width))
        })
        .collect();

    for path in &paths {
        guard_output(path, force)?;
    }

    std::fs::create_dir_all(&args.output)?;

    for (page, path) in selected.iter().zip(&paths) {
        let rendered = renderer.render(*page, Scale::Dpi(args.dpi))?;
        write_png(&rendered, path)?;
        println!("{}", path.display());
    }

    Ok(())
}

pub fn thumbnail(args: ThumbnailArgs, force: bool) -> Result<()> {
    if args.page == 0 {
        anyhow::bail!("page numbers start at 1");
    }
    if args.size == 0 {
        anyhow::bail!("--size must be at least 1");
    }

    guard_output(&args.output, force)?;

    let renderer = PageRenderer::open(&args.input)?;
    let rendered = renderer.thumbnail(args.page - 1, args.size)?;
    write_png(&rendered, &args.output)?;

    println!("{}", args.output.display());
    Ok(())
}

fn write_png(page: &RenderedPage, path: &std::path::Path) -> Result<()> {
    let colour = match page.channels {
        4 => ExtendedColorType::Rgba8,
        3 => ExtendedColorType::Rgb8,
        1 => ExtendedColorType::L8,
        other => anyhow::bail!("cannot write a PNG with {other} channels per pixel"),
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let file = std::fs::File::create(path)
        .with_context(|| format!("could not create {}", path.display()))?;

    image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file))
        .write_image(&page.pixels, page.width, page.height, colour)
        .with_context(|| format!("could not encode {}", path.display()))?;

    Ok(())
}
