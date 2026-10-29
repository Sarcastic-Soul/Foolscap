//! `foolscap articles` — list the article threads of a document.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use pdf_core::{list_articles, Document};

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to read.
    input: PathBuf,
}

pub fn run(args: Args, password: Option<&str>) -> Result<()> {
    let doc = Document::open_with(&args.input, password)?;
    let articles = list_articles(&doc)?;
    if articles.is_empty() {
        println!("no article threads");
    }
    for article in &articles {
        let start = article
            .first_page
            .map_or("no page".to_string(), |page| format!("from page {page}"));
        println!("{}  {} box(es), {start}", article.title, article.beads);
    }
    Ok(())
}
