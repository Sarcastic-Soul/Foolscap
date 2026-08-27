use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::Document;

use crate::output::human_bytes;

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to inspect.
    input: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let doc = Document::open(&args.input)?;
    let size = std::fs::metadata(&args.input).map(|m| m.len()).unwrap_or(0);
    let metadata = doc.metadata()?;

    println!("file       {}", args.input.display());
    println!("size       {}", human_bytes(size));
    println!("pages      {}", doc.page_count());
    println!("version    {}", doc.version());

    let fields = [
        ("title", &metadata.title),
        ("author", &metadata.author),
        ("subject", &metadata.subject),
        ("keywords", &metadata.keywords),
        ("creator", &metadata.creator),
        ("producer", &metadata.producer),
    ];

    for (name, value) in fields {
        if let Some(value) = value {
            println!("{name:<10} {value}");
        }
    }

    Ok(())
}
