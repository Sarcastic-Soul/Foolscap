use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgGroup, Parser};
use pdf_core::{PageRange, SplitOptions, SplitSpec};

use crate::output::guard_output;

#[derive(Debug, Parser)]
#[command(group(
    ArgGroup::new("mode").required(true)
))]
pub struct Args {
    /// The document to split.
    input: PathBuf,

    /// Pages to extract into a single file, such as 1-3,7 or 9-.
    #[arg(short, long, group = "mode")]
    pages: Option<String>,

    /// Burst into consecutive chunks of this many pages.
    #[arg(short, long, group = "mode")]
    every: Option<usize>,

    /// Split at each top-level bookmark (outline entry).
    #[arg(long, group = "mode")]
    bookmarks: bool,

    /// Pack pages greedily into chunks not exceeding this many megabytes.
    #[arg(long, value_name = "MB", group = "mode")]
    max_size: Option<u64>,

    /// Burst every N pages (synonym for --every).
    #[arg(long, value_name = "N", group = "mode")]
    by_pages: Option<usize>,

    /// Directory to write the results into.
    #[arg(short, long)]
    output: PathBuf,

    /// Output file name pattern. Supports {stem}, {index}, {width}, {pages},
    /// and {index:0{width}d}.
    #[arg(long, value_name = "PATTERN")]
    output_pattern: Option<String>,

    /// Bundle all output pieces into a single zip archive at output/{stem}.zip.
    #[arg(long)]
    zip: bool,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    let spec = match (
        args.pages,
        args.every,
        args.bookmarks,
        args.max_size,
        args.by_pages,
    ) {
        (Some(spec), _, _, _, _) => SplitSpec::Extract(PageRange::parse(&spec)?),
        (_, Some(size), _, _, _) => SplitSpec::Every(size),
        (_, _, true, _, _) => SplitSpec::ByBookmarks,
        (_, _, _, Some(mb), _) => SplitSpec::BySize(mb),
        (_, _, _, _, Some(n)) => SplitSpec::ByPages(n),
        // clap's ArgGroup guarantees one of the modes is present.
        (None, None, false, None, None) => unreachable!("argument group requires a split mode"),
    };

    let options = SplitOptions {
        output_pattern: args.output_pattern,
        zip: args.zip,
    };

    // Check every output name before writing any of them, so that a collision
    // on the last piece does not leave the earlier ones already overwritten.
    for path in
        pdf_core::split_plan_with_options(&args.input, &spec, &args.output, password, &options)?
    {
        guard_output(&path, force)?;
    }

    std::fs::create_dir_all(&args.output)?;

    let mut report = |progress: pdf_core::Progress| {
        tracing::info!("{}", progress.message);
    };

    let written = pdf_core::split_with_progress(
        &args.input,
        &spec,
        &args.output,
        password,
        Some(&mut report),
        &options,
    )?;

    for path in &written {
        println!("{}", path.display());
    }

    Ok(())
}
