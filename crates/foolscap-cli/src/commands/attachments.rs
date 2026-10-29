//! `foolscap attachments` — list, add, and extract embedded files.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use pdf_core::{add_attachment, extract_attachment, list_attachments, Document};

use crate::output::{count, guard_output};

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Command,

    /// The document to operate on.
    input: PathBuf,

    /// Where to write the document (required for add).
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the embedded files.
    List,

    /// Embed a file in the document.
    Add {
        /// The file to embed.
        file: PathBuf,

        /// The name it carries inside the document; the file's own name by default.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },

    /// Write one embedded file out.
    Extract {
        /// The attachment's name, as `list` prints it.
        name: String,

        /// Where to write the bytes.
        #[arg(short, long)]
        output: PathBuf,
    },
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    match &args.command {
        Command::List => {
            let doc = Document::open_with(&args.input, password)?;
            let attachments = list_attachments(&doc)?;
            if attachments.is_empty() {
                println!("no attachments");
            }
            for attachment in &attachments {
                println!("{}  {} bytes", attachment.name, attachment.size);
            }
            Ok(())
        }
        Command::Add { file, name } => {
            let output = args
                .output
                .as_ref()
                .ok_or_else(|| anyhow!("--output is required for add"))?;
            guard_output(output, force)?;

            let name = match name {
                Some(name) => name.clone(),
                None => file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .ok_or_else(|| anyhow!("{} has no file name; pass --name", file.display()))?,
            };
            let data = std::fs::read(file)
                .with_context(|| format!("could not read {}", file.display()))?;

            let mut doc = Document::open_with(&args.input, password)?;
            add_attachment(&mut doc, &name, &data)?;
            doc.save(output)?;
            println!("embedded {name} ({})", count(data.len(), "byte"));
            println!("{}", output.display());
            Ok(())
        }
        Command::Extract { name, output } => {
            guard_output(output, force)?;
            let doc = Document::open_with(&args.input, password)?;
            let bytes = extract_attachment(&doc, name)?;
            let mut file = std::fs::File::create(output)
                .with_context(|| format!("could not create {}", output.display()))?;
            file.write_all(&bytes)?;
            println!("{}", output.display());
            Ok(())
        }
    }
}
