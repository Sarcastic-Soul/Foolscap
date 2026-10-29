use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use pdf_core::{
    add_bookmark, export_bookmarks, import_bookmarks, list_bookmarks, Bookmark, Document,
};

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Command,

    /// The document to operate on.
    input: PathBuf,

    /// Where to write the result (required for add/import).
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List all bookmarks in the document.
    List,

    /// Add a bookmark to the document.
    Add {
        /// Title of the bookmark.
        #[arg(long, value_name = "TEXT")]
        title: String,

        /// 1-indexed page number the bookmark points to.
        #[arg(long, value_name = "N")]
        page: usize,

        /// Parent bookmark index (0 = top-level). Use `list` to see indices.
        #[arg(long, value_name = "N", default_value = "0")]
        parent: usize,
    },

    /// Import bookmarks from a JSON file, replacing existing ones.
    Import {
        /// JSON file containing bookmarks to import.
        file: PathBuf,
    },

    /// Export bookmarks to a JSON file.
    Export {
        /// Output JSON file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    match &args.command {
        Command::List => run_list(&args, password),
        Command::Add {
            title,
            page,
            parent,
        } => run_add(&args, title.clone(), *page, *parent, force, password),
        Command::Import { file } => run_import(&args, file.clone(), force, password),
        Command::Export { output } => run_export(&args, output.clone(), force, password),
    }
}

fn run_list(args: &Args, password: Option<&str>) -> Result<()> {
    let doc = Document::open_with(&args.input, password)?;
    let bookmarks = list_bookmarks(&doc)?;

    if bookmarks.is_empty() {
        println!("No bookmarks found.");
        return Ok(());
    }

    print_bookmarks(&bookmarks, 0, &mut 0);
    Ok(())
}

fn print_bookmarks(bookmarks: &[Bookmark], depth: usize, counter: &mut usize) {
    let indent = "  ".repeat(depth);
    for bm in bookmarks {
        println!("{}{} [{}] page {}", indent, *counter, bm.title, bm.page);
        *counter += 1;
        if !bm.children.is_empty() {
            print_bookmarks(&bm.children, depth + 1, counter);
        }
    }
}

fn run_add(
    args: &Args,
    title: String,
    page: usize,
    parent: usize,
    force: bool,
    password: Option<&str>,
) -> Result<()> {
    let output = args
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--output is required for add"))?;
    guard_output(output, force)?;

    let mut doc = Document::open_with(&args.input, password)?;

    if page == 0 || page > doc.page_count() {
        bail!("page must be between 1 and {}", doc.page_count());
    }

    let bookmark = Bookmark::new(title, page);
    add_bookmark(&mut doc, bookmark, parent)?;

    doc.save(output)?;
    println!("{}", output.display());
    Ok(())
}

fn run_import(args: &Args, file: PathBuf, force: bool, password: Option<&str>) -> Result<()> {
    let output = args
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--output is required for import"))?;
    guard_output(output, force)?;

    let json = std::fs::read_to_string(&file)?;
    let bookmarks: Vec<Bookmark> = serde_json::from_str(&json)?;

    // Validate page numbers.
    let mut doc = Document::open_with(&args.input, password)?;
    let page_count = doc.page_count();

    fn validate(bookmarks: &[Bookmark], page_count: usize) -> Result<()> {
        for bm in bookmarks {
            if bm.page == 0 || bm.page > page_count {
                bail!(
                    "bookmark '{}' has invalid page {} (must be 1..={})",
                    bm.title,
                    bm.page,
                    page_count
                );
            }
            validate(&bm.children, page_count)?;
        }
        Ok(())
    }
    validate(&bookmarks, page_count)?;

    import_bookmarks(&mut doc, bookmarks)?;
    doc.save(output)?;
    println!("{}", output.display());
    Ok(())
}

fn run_export(
    args: &Args,
    output: Option<PathBuf>,
    force: bool,
    password: Option<&str>,
) -> Result<()> {
    let doc = Document::open_with(&args.input, password)?;
    let bookmarks = export_bookmarks(&doc)?;

    let json = serde_json::to_string_pretty(&bookmarks)?;

    match output {
        Some(path) => {
            guard_output(&path, force)?;
            std::fs::write(&path, json)?;
            println!("{}", path.display());
        }
        None => {
            println!("{}", json);
        }
    }
    Ok(())
}
