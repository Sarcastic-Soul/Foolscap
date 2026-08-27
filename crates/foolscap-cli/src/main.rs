//! Foolscap command-line interface.
//!
//! This layer parses arguments, installs a `tracing` subscriber, calls into
//! `pdf-core`, and formats the result. It holds no PDF logic of its own.

mod commands;
mod output;

use anyhow::Result;
use clap::{Parser, Subcommand};

use commands::{info, merge, meta, optimize, rotate, split};

#[derive(Debug, Parser)]
#[command(
    name = "foolscap",
    version,
    about = "A Linux-first PDF toolkit",
    long_about = None,
)]
struct Cli {
    /// Increase log verbosity. Repeat for more detail.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Overwrite output files that already exist.
    #[arg(short, long, global = true)]
    force: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Concatenate documents into one.
    Merge(merge::Args),
    /// Extract pages, or burst a document into pieces.
    Split(split::Args),
    /// Rotate pages in place.
    Rotate(rotate::Args),
    /// Show page count, size, and metadata.
    Info(info::Args),
    /// Read or edit the document information dictionary.
    Meta(meta::Args),
    /// Losslessly shrink a document.
    Optimize(optimize::Args),
    /// Show which optional features this build supports.
    Capabilities,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let force = cli.force;

    match cli.command {
        Command::Merge(args) => merge::run(args, force),
        Command::Split(args) => split::run(args, force),
        Command::Rotate(args) => rotate::run(args, force),
        Command::Info(args) => info::run(args),
        Command::Meta(args) => meta::run(args, force),
        Command::Optimize(args) => optimize::run(args, force),
        Command::Capabilities => {
            output::print_capabilities();
            Ok(())
        }
    }
}

fn init_tracing(verbosity: u8) {
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}
