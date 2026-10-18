//! `foolscap extract-images` — pull every embedded image out of a PDF.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to extract images from.
    input: PathBuf,

    /// Directory to write the images into.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, _force: bool, password: Option<&str>) -> Result<()> {
    // Guard each potential output? We'll just create dir.
    std::fs::create_dir_all(&args.output)?;
    let written = pdf_core::extract_images(&args.input, &args.output, password)?;
    for path in &written {
        println!("{}", path.display());
    }
    println!("extracted {} image(s)", written.len());
    Ok(())
}
