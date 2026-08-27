use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;
use pdf_core::{Document, MetadataEdit};

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to edit.
    input: PathBuf,

    #[arg(long, value_name = "TEXT")]
    set_title: Option<String>,
    #[arg(long, value_name = "TEXT")]
    set_author: Option<String>,
    #[arg(long, value_name = "TEXT")]
    set_subject: Option<String>,
    #[arg(long, value_name = "TEXT")]
    set_keywords: Option<String>,

    /// Remove the title.
    #[arg(long, conflicts_with = "set_title")]
    clear_title: bool,
    /// Remove the author.
    #[arg(long, conflicts_with = "set_author")]
    clear_author: bool,
    /// Remove the subject.
    #[arg(long, conflicts_with = "set_subject")]
    clear_subject: bool,
    /// Remove the keywords.
    #[arg(long, conflicts_with = "set_keywords")]
    clear_keywords: bool,

    /// Where to write the result.
    #[arg(short, long)]
    output: PathBuf,
}

impl Args {
    fn edit(&self) -> MetadataEdit {
        fn field(set: &Option<String>, clear: bool) -> Option<Option<String>> {
            match (set, clear) {
                (Some(value), _) => Some(Some(value.clone())),
                (None, true) => Some(None),
                (None, false) => None,
            }
        }

        MetadataEdit {
            title: field(&self.set_title, self.clear_title),
            author: field(&self.set_author, self.clear_author),
            subject: field(&self.set_subject, self.clear_subject),
            keywords: field(&self.set_keywords, self.clear_keywords),
        }
    }
}

pub fn run(args: Args, force: bool) -> Result<()> {
    let edit = args.edit();
    if edit.is_empty() {
        bail!("nothing to change; pass at least one --set-* or --clear-* option");
    }

    guard_output(&args.output, force)?;

    let mut doc = Document::open(&args.input)?;
    let updated = edit.apply(&doc.metadata()?);
    doc.set_metadata(&updated)?;
    doc.save(&args.output)?;

    println!("{}", args.output.display());
    Ok(())
}
