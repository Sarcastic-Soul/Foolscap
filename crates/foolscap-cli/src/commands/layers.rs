//! `foolscap layers` — list optional content, and show or hide a layer.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use pdf_core::{list_layers, set_layer_visibility, Document};

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Command,

    /// The document to operate on.
    input: PathBuf,

    /// Where to write the document (required for show and hide).
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the layers and whether each is shown.
    List,

    /// Show a hidden layer.
    Show {
        /// The layer's name, as `list` prints it.
        name: String,
    },

    /// Hide a layer.
    Hide {
        /// The layer's name, as `list` prints it.
        name: String,
    },
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    match &args.command {
        Command::List => {
            let doc = Document::open_with(&args.input, password)?;
            let layers = list_layers(&doc)?;
            if layers.is_empty() {
                println!("no layers");
            }
            for layer in &layers {
                let state = if layer.visible { "shown" } else { "hidden" };
                println!("{:<6}  {}", state, layer.name);
            }
            Ok(())
        }
        Command::Show { name } | Command::Hide { name } => {
            let visible = matches!(args.command, Command::Show { .. });
            let output = args
                .output
                .as_ref()
                .ok_or_else(|| anyhow!("--output is required to change a layer"))?;
            guard_output(output, force)?;

            let mut doc = Document::open_with(&args.input, password)?;
            set_layer_visibility(&mut doc, name, visible)?;
            doc.save(output)?;
            println!("{}", output.display());
            Ok(())
        }
    }
}
