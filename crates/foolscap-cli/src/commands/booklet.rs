use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Result;
use clap::Parser;
use pdf_core::{Binding, BookletOptions, Document, Fit, Margins, PageSize};

use crate::output::{guard_output, pages};

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to impose as a booklet.
    input: PathBuf,

    /// Pages per signature (must be a multiple of 4, e.g., 4, 8, 16).
    #[arg(short, long, default_value = "8")]
    signature: usize,

    /// Output paper size (the sheet size): a4, letter, legal, or WxH in points.
    #[arg(short, long, default_value = "a4")]
    paper: String,

    /// Binding edge for duplex printing: short-edge or long-edge.
    #[arg(long, default_value = "short-edge")]
    binding: String,

    /// Margins around the 2×1 grid on each sheet in points:
    /// top,bottom,left,right or a single value for all sides.
    #[arg(long, default_value = "0")]
    margins: String,

    /// Gap between the two pages on a sheet in points.
    #[arg(long, default_value = "0")]
    gap: f32,

    /// Draw a border around each placed page.
    #[arg(long)]
    border: bool,

    /// Scale mode: contain, cover, or stretch.
    #[arg(long, default_value = "contain")]
    fit: String,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let doc = Document::open_with(&args.input, password)?;
    let before = doc.page_count();

    // Parse paper size
    let paper_size = PageSize::parse(&args.paper)?;

    // Parse binding
    let binding = Binding::from_str(&args.binding)?;

    // Parse margins
    let margins = parse_margins(&args.margins)?;

    // Parse fit
    let fit = Fit::parse(&args.fit)?;

    let options = BookletOptions {
        signature_size: args.signature,
        paper_size,
        binding,
        margins,
        gap: args.gap,
        border: args.border,
        fit,
    };

    let mut imposed = pdf_core::booklet(&doc, options)?;
    let after = imposed.page_count();
    imposed.save(&args.output)?;

    println!("{} pages -> {} sheets", pages(before), pages(after));
    println!("{}", args.output.display());
    Ok(())
}

fn parse_margins(spec: &str) -> Result<Margins> {
    let parts: Vec<f32> = spec
        .split([',', ' '])
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().parse())
        .collect::<Result<Vec<_>, _>>()?;

    match parts.len() {
        1 => Ok(Margins {
            top: parts[0],
            bottom: parts[0],
            left: parts[0],
            right: parts[0],
        }),
        4 => Ok(Margins {
            top: parts[0],
            bottom: parts[1],
            left: parts[2],
            right: parts[3],
        }),
        _ => Err(anyhow::anyhow!(
            "margins must be a single value or four values (top,bottom,left,right)"
        )),
    }
}
