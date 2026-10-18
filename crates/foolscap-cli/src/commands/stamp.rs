//! Stamping: one engine with four faces.
//!
//! `stamp` is the general form; `watermark`, `page-numbers` and
//! `header-footer` are thin presets over the same [`pdf_core`] call.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;
use pdf_core::{Anchor, Document, PageRange, StampContent, StampFont, StampLayer, StampOptions};

use crate::output::guard_output;

/// Draw text or an image over or under pages.
#[derive(Debug, Parser)]
pub struct StampArgs {
    /// The document to stamp.
    input: PathBuf,

    /// Pages to stamp, such as 1-3,7 or all. Defaults to every page.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Text to draw. Exactly one of --text or --image is required.
    #[arg(long, conflicts_with = "image")]
    text: Option<String>,

    /// Image file (PNG or JPEG) to draw. Exactly one of --text or --image.
    #[arg(long, value_name = "PATH")]
    image: Option<PathBuf>,

    /// Where on the page to draw.
    #[arg(long, default_value = "center")]
    anchor: String,

    /// Extra horizontal shift in points.
    #[arg(long, default_value_t = 0.0)]
    dx: f32,

    /// Extra vertical shift in points.
    #[arg(long, default_value_t = 0.0)]
    dy: f32,

    /// Inset from the page edges for edge anchors, in points.
    #[arg(long, default_value_t = 36.0)]
    margin: f32,

    /// Counter-clockwise rotation in degrees.
    #[arg(long, default_value_t = 0.0)]
    rotation: f32,

    /// Opacity from 0 (invisible) to 1 (solid).
    #[arg(long, default_value_t = 1.0)]
    opacity: f32,

    /// Text size in points.
    #[arg(long, default_value_t = 12.0)]
    font_size: f32,

    /// Text colour as #rrggbb or r,g,b with channels in 0..=1.
    #[arg(long, default_value = "black")]
    color: String,

    /// Text font: helvetica or courier.
    #[arg(long, default_value = "helvetica")]
    font: String,

    /// Draw over the page content, or under it.
    #[arg(long, default_value = "over")]
    layer: String,

    /// Image size as a multiple of its pixel size at 72 dpi.
    #[arg(long, default_value_t = 1.0)]
    scale: f32,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

/// Draw a large translucent mark, diagonally by default.
#[derive(Debug, Parser)]
pub struct WatermarkArgs {
    /// The document to mark.
    input: PathBuf,

    /// Pages to mark. Defaults to every page.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Text to draw, unless --image is given.
    #[arg(long)]
    text: Option<String>,

    /// Image file (PNG or JPEG) to draw instead of text.
    #[arg(long, value_name = "PATH")]
    image: Option<PathBuf>,

    /// Where on the page to draw.
    #[arg(long, default_value = "center")]
    anchor: String,

    /// Counter-clockwise rotation in degrees.
    #[arg(long, default_value_t = 45.0)]
    rotation: f32,

    /// Opacity from 0 (invisible) to 1 (solid).
    #[arg(long, default_value_t = 0.25)]
    opacity: f32,

    /// Text size in points.
    #[arg(long, default_value_t = 48.0)]
    font_size: f32,

    /// Text colour as #rrggbb or r,g,b with channels in 0..=1.
    #[arg(long, default_value = "gray")]
    color: String,

    /// Draw over the page content, or under it.
    #[arg(long, default_value = "under")]
    layer: String,

    /// Image size as a multiple of its pixel size at 72 dpi.
    #[arg(long, default_value_t = 1.0)]
    scale: f32,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

/// Draw running page numbers. `{n}` is the page number, `{total}` the count.
#[derive(Debug, Parser)]
pub struct PageNumbersArgs {
    /// The document to number.
    input: PathBuf,

    /// Pages to number. Defaults to every page.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Number template, with {n} and {total}.
    #[arg(long, default_value = "Page {n} of {total}")]
    format: String,

    /// Where on the page to draw.
    #[arg(long, default_value = "bottom-center")]
    anchor: String,

    /// Inset from the page edges for edge anchors, in points.
    #[arg(long, default_value_t = 36.0)]
    margin: f32,

    /// Text size in points.
    #[arg(long, default_value_t = 10.0)]
    font_size: f32,

    /// Text colour as #rrggbb or r,g,b with channels in 0..=1.
    #[arg(long, default_value = "black")]
    color: String,

    /// Draw over the page content, or under it.
    #[arg(long, default_value = "over")]
    layer: String,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

/// Draw a running header and/or footer. Both accept {n} and {total}.
#[derive(Debug, Parser)]
pub struct HeaderFooterArgs {
    /// The document to stamp.
    input: PathBuf,

    /// Pages to stamp. Defaults to every page.
    #[arg(short, long, default_value = "all")]
    pages: String,

    /// Header template drawn at the top. Omit for no header.
    #[arg(long)]
    header: Option<String>,

    /// Footer template drawn at the bottom. Omit for no footer.
    #[arg(long)]
    footer: Option<String>,

    /// Inset from the page edges, in points.
    #[arg(long, default_value_t = 36.0)]
    margin: f32,

    /// Text size in points.
    #[arg(long, default_value_t = 10.0)]
    font_size: f32,

    /// Text colour as #rrggbb or r,g,b with channels in 0..=1.
    #[arg(long, default_value = "black")]
    color: String,

    /// Opacity from 0 (invisible) to 1 (solid).
    #[arg(long, default_value_t = 1.0)]
    opacity: f32,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn stamp(args: StampArgs, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let content = match (args.text, args.image) {
        (Some(text), None) => StampContent::Text(text),
        (None, Some(path)) => StampContent::Image(std::fs::read(&path)?),
        (None, None) => bail!("pass exactly one of --text or --image"),
        (Some(_), Some(_)) => bail!("pass exactly one of --text or --image"),
    };

    let opts = StampOptions {
        content,
        anchor: args.anchor.parse::<Anchor>()?,
        offset: (args.dx, args.dy),
        margin: args.margin,
        rotation_deg: args.rotation,
        opacity: args.opacity,
        font_size: args.font_size,
        color: parse_color(&args.color)?,
        font: args.font.parse::<StampFont>()?,
        layer: args.layer.parse::<StampLayer>()?,
        image_scale: args.scale,
        ..Default::default()
    };

    run(&args.input, &args.output, &args.pages, &opts, password)?;
    println!("{}", args.output.display());
    Ok(())
}

pub fn watermark(args: WatermarkArgs, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let content = match (args.text, args.image) {
        (Some(text), None) => StampContent::Text(text),
        (None, Some(path)) => StampContent::Image(std::fs::read(&path)?),
        (None, None) => bail!("pass --text or --image"),
        (Some(_), Some(_)) => bail!("pass exactly one of --text or --image"),
    };

    let opts = StampOptions {
        content,
        anchor: args.anchor.parse::<Anchor>()?,
        rotation_deg: args.rotation,
        opacity: args.opacity,
        font_size: args.font_size,
        color: parse_color(&args.color)?,
        layer: args.layer.parse::<StampLayer>()?,
        image_scale: args.scale,
        ..StampOptions::default()
    };

    run(&args.input, &args.output, &args.pages, &opts, password)?;
    println!("{}", args.output.display());
    Ok(())
}

pub fn page_numbers(args: PageNumbersArgs, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let opts = StampOptions {
        content: StampContent::Text(args.format.clone()),
        anchor: args.anchor.parse::<Anchor>()?,
        margin: args.margin,
        font_size: args.font_size,
        color: parse_color(&args.color)?,
        layer: args.layer.parse::<StampLayer>()?,
        ..StampOptions::default()
    };

    run(&args.input, &args.output, &args.pages, &opts, password)?;
    println!("{}", args.output.display());
    Ok(())
}

pub fn header_footer(args: HeaderFooterArgs, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    if args.header.is_none() && args.footer.is_none() {
        bail!("pass at least one of --header or --footer");
    }

    let range = PageRange::parse(&args.pages)?;
    let mut doc = Document::open_with(&args.input, password)?;
    let color = parse_color(&args.color)?;

    if let Some(header) = &args.header {
        let opts = StampOptions {
            content: StampContent::Text(header.clone()),
            anchor: Anchor::TopCenter,
            margin: args.margin,
            font_size: args.font_size,
            color,
            opacity: args.opacity,
            ..StampOptions::default()
        };
        pdf_core::stamp(&mut doc, &range, &opts)?;
    }

    if let Some(footer) = &args.footer {
        let opts = StampOptions {
            content: StampContent::Text(footer.clone()),
            anchor: Anchor::BottomCenter,
            margin: args.margin,
            font_size: args.font_size,
            color,
            opacity: args.opacity,
            ..StampOptions::default()
        };
        pdf_core::stamp(&mut doc, &range, &opts)?;
    }

    doc.save(&args.output)?;
    println!("{}", args.output.display());
    Ok(())
}

fn run(
    input: &PathBuf,
    output: &PathBuf,
    pages: &str,
    opts: &StampOptions,
    password: Option<&str>,
) -> Result<()> {
    let range = PageRange::parse(pages)?;
    let mut doc = Document::open_with(input, password)?;
    pdf_core::stamp(&mut doc, &range, opts)?;
    doc.save(output)?;
    Ok(())
}

/// Colours as `#rrggbb`, a few names, or `r,g,b` with channels in `0..=1`.
fn parse_color(spec: &str) -> Result<(f32, f32, f32)> {
    let lower = spec.to_ascii_lowercase();
    match lower.as_str() {
        "black" => return Ok((0.0, 0.0, 0.0)),
        "white" => return Ok((1.0, 1.0, 1.0)),
        "gray" | "grey" => return Ok((0.5, 0.5, 0.5)),
        "red" => return Ok((1.0, 0.0, 0.0)),
        "green" => return Ok((0.0, 0.5, 0.0)),
        "blue" => return Ok((0.0, 0.0, 1.0)),
        _ => {}
    }

    if let Some(hex) = spec.strip_prefix('#') {
        if hex.len() == 6 {
            let channel = |range: std::ops::Range<usize>| -> Result<f32> {
                u8::from_str_radix(&hex[range], 16)
                    .map(|v| f32::from(v) / 255.0)
                    .map_err(|_| anyhow::anyhow!("{spec:?} is not a colour; expected #rrggbb"))
            };
            return Ok((channel(0..2)?, channel(2..4)?, channel(4..6)?));
        }
        bail!("{spec:?} is not a colour; expected #rrggbb or r,g,b");
    }

    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() == 3 {
        let mut channels = [0.0f32; 3];
        for (index, part) in parts.iter().enumerate() {
            channels[index] = part
                .trim()
                .parse::<f32>()
                .map_err(|_| anyhow::anyhow!("{spec:?} is not a colour; expected r,g,b"))?;
        }
        return Ok((channels[0], channels[1], channels[2]));
    }

    bail!("{spec:?} is not a colour; expected a name, #rrggbb, or r,g,b")
}
