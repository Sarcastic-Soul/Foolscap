//! `foolscap page-labels` — read or set the page numbers a viewer shows.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use pdf_core::{page_labels, set_page_labels, Document, LabelRange, LabelStyle};

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Command,

    /// The document to operate on.
    input: PathBuf,

    /// Where to write the document (required for set and clear).
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the label of every page.
    Get,

    /// Replace the labels with numbered ranges.
    Set {
        /// A range as START:STYLE[:PREFIX[:FIRST]]. START is the first page
        /// (1-indexed). STYLE is one of decimal, upper-roman, lower-roman,
        /// upper-alpha, lower-alpha, or none. Repeat the flag for more ranges.
        #[arg(long = "range", value_name = "SPEC", required = true)]
        ranges: Vec<String>,
    },

    /// Remove the labels, so pages show plain 1, 2, 3.
    Clear,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    match &args.command {
        Command::Get => {
            let doc = Document::open_with(&args.input, password)?;
            for label in page_labels(&doc)? {
                println!("{label}");
            }
            Ok(())
        }
        Command::Set { ranges } => {
            let parsed = ranges
                .iter()
                .map(|spec| parse_range(spec))
                .collect::<Result<Vec<_>>>()?;
            write_labels(&args, force, password, &parsed)
        }
        Command::Clear => write_labels(&args, force, password, &[]),
    }
}

fn write_labels(
    args: &Args,
    force: bool,
    password: Option<&str>,
    ranges: &[LabelRange],
) -> Result<()> {
    let output = args
        .output
        .as_ref()
        .ok_or_else(|| anyhow!("--output is required to change page labels"))?;
    guard_output(output, force)?;

    let mut doc = Document::open_with(&args.input, password)?;
    set_page_labels(&mut doc, ranges)?;
    doc.save(output)?;
    println!("{}", output.display());
    Ok(())
}

/// Parse `START:STYLE[:PREFIX[:FIRST]]`.
fn parse_range(spec: &str) -> Result<LabelRange> {
    let parts: Vec<&str> = spec.splitn(4, ':').collect();
    if parts.len() < 2 {
        bail!("range {spec:?} needs at least START:STYLE");
    }
    let start_page: usize = parts[0]
        .parse()
        .with_context(|| format!("range {spec:?}: START must be a page number"))?;
    let style = LabelStyle::parse(parts[1]).ok_or_else(|| {
        anyhow!(
            "range {spec:?}: unknown style {:?}; use decimal, upper-roman, lower-roman, upper-alpha, lower-alpha or none",
            parts[1]
        )
    })?;
    let prefix = parts.get(2).copied().unwrap_or("").to_string();
    let first: u32 = match parts.get(3) {
        Some(value) => value
            .parse()
            .with_context(|| format!("range {spec:?}: FIRST must be a number"))?,
        None => 1,
    };
    Ok(LabelRange {
        start_page,
        style,
        prefix,
        first,
    })
}
