use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use pdf_core::{to_markdown, Document, MarkdownOptions, PageRange};

use crate::output::guard_output;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The document to extract from.
    input: PathBuf,

    /// Pages to extract (e.g. 1-3,7); all pages by default.
    #[arg(long)]
    pages: Option<String>,

    /// Where to write the Markdown; stdout when omitted.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    // Parsed before anything is read, so a bad spec fails before work starts.
    let pages = args.pages.as_deref().map(PageRange::parse).transpose()?;

    let options = MarkdownOptions {
        pages,
        ..MarkdownOptions::default()
    };

    let doc = Document::open_with(&args.input, password)?;
    let markdown = to_markdown(&doc, &options)?;

    match &args.output {
        Some(path) => {
            guard_output(path, force)?;
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(path, markdown)?;
            println!("{}", path.display());
        }
        None => print!("{markdown}"),
    }

    Ok(())
}
