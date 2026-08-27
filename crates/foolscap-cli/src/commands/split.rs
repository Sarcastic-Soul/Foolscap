use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgGroup, Parser};
use pdf_core::{PageRange, SplitSpec};

use crate::output::guard_output;

#[derive(Debug, Parser)]
#[command(group(
    ArgGroup::new("mode").required(true).args(["pages", "every"])
))]
pub struct Args {
    /// The document to split.
    input: PathBuf,

    /// Pages to extract into a single file, such as 1-3,7 or 9-.
    #[arg(short, long)]
    pages: Option<String>,

    /// Burst into consecutive chunks of this many pages.
    #[arg(short, long)]
    every: Option<usize>,

    /// Directory to write the results into.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool) -> Result<()> {
    let spec = match (&args.pages, args.every) {
        (Some(spec), _) => SplitSpec::Extract(PageRange::parse(spec)?),
        (None, Some(size)) => SplitSpec::Every(size),
        // clap's ArgGroup guarantees one of the two is present.
        (None, None) => unreachable!("argument group requires --pages or --every"),
    };

    // Check every output name before writing any of them, so that a collision
    // on the last piece does not leave the earlier ones already overwritten.
    for path in pdf_core::split_plan(&args.input, &spec, &args.output)? {
        guard_output(&path, force)?;
    }

    std::fs::create_dir_all(&args.output)?;

    let mut report = |progress: pdf_core::Progress| {
        tracing::info!("{}", progress.message);
    };

    let written =
        pdf_core::split_with_progress(&args.input, &spec, &args.output, Some(&mut report))?;

    for path in &written {
        println!("{}", path.display());
    }

    Ok(())
}
