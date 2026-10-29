use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use pdf_core::ops::svg::{plan, to_svg_with_progress, SvgOptions};
use pdf_core::PageRange;

use crate::output::guard_output;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The document to export.
    input: PathBuf,

    /// Pages to export (e.g. 1-3,7); all pages by default.
    #[arg(long)]
    pages: Option<String>,

    /// Output directory; defaults to one named after the input file.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    // Parsed before anything is read, so a bad spec fails before work starts.
    let pages = args.pages.as_deref().map(PageRange::parse).transpose()?;

    let options = SvgOptions {
        pages: pages.unwrap_or(PageRange::All),
        password: password.map(str::to_string),
    };

    // The plan says what would be written; guard every path before any of it
    // reaches the disk, so a collision leaves nothing behind.
    let out_dir = args
        .output
        .unwrap_or_else(|| default_directory(&args.input));
    for path in plan(&args.input, &out_dir, &options)? {
        guard_output(&path, force)?;
    }

    let mut report = |progress: pdf_core::Progress| tracing::info!("{}", progress.message);
    let written = to_svg_with_progress(&args.input, &out_dir, &options, Some(&mut report))?;

    for path in &written {
        println!("{}", path.display());
    }
    Ok(())
}

/// A directory beside the input, named after the file's stem: exporting
/// `scans/report.pdf` writes `scans/report/report-1.svg` and friends, which
/// keeps one export out of another's way.
fn default_directory(input: &std::path::Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|stem| PathBuf::from(stem.to_os_string()))
        .unwrap_or_else(|| PathBuf::from("pages"));
    input
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""))
        .join(stem)
}
