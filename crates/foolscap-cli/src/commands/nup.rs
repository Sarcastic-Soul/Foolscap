use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Result;
use clap::Parser;
use pdf_core::{Document, Fit, Margins, NupOptions, PageOrder, PageSize};

use crate::output::{guard_output, pages};

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to impose.
    input: PathBuf,

    /// Grid layout as ROWSxCOLS (e.g., 2x2, 3x2).
    #[arg(short, long, default_value = "2x2")]
    layout: String,

    /// Output paper size: a4, letter, legal, or WxH in points (e.g., 595x842).
    #[arg(short, long, default_value = "a4")]
    paper: String,

    /// Margins around the grid in points: top,bottom,left,right or a single value for all sides.
    #[arg(long, default_value = "0")]
    margins: String,

    /// Gap between grid cells in points.
    #[arg(long, default_value = "0")]
    gap: f32,

    /// Draw a border around each placed page.
    #[arg(long)]
    border: bool,

    /// Page order within the grid: row (row-major) or column (column-major).
    #[arg(long, default_value = "row")]
    order: String,

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

    // Parse layout
    let (rows, cols) = parse_layout(&args.layout)?;

    // Parse paper size
    let paper_size = PageSize::parse(&args.paper)?;

    // Parse margins
    let margins = parse_margins(&args.margins)?;

    // Parse page order
    let page_order = PageOrder::from_str(&args.order)?;

    // Parse fit
    let fit = Fit::parse(&args.fit)?;

    let options = NupOptions {
        rows,
        cols,
        paper_size,
        margins,
        gap: args.gap,
        border: args.border,
        page_order,
        fit,
    };

    let mut imposed = pdf_core::nup(&doc, options)?;
    let after = imposed.page_count();
    imposed.save(&args.output)?;

    println!("{} -> {}", pages(before), pages(after));
    println!("{}", args.output.display());
    Ok(())
}

fn parse_layout(spec: &str) -> Result<(usize, usize)> {
    let parts: Vec<&str> = spec.split('x').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!("layout must be ROWSxCOLS (e.g., 2x2)"));
    }
    let rows = parts[0].parse::<usize>()?;
    let cols = parts[1].parse::<usize>()?;
    if rows == 0 || cols == 0 {
        return Err(anyhow::anyhow!("rows and cols must be at least 1"));
    }
    if rows > 10 || cols > 10 {
        return Err(anyhow::anyhow!("rows and cols must not exceed 10"));
    }
    Ok((rows, cols))
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
