//! Foolscap command-line interface.
//!
//! This layer parses arguments, installs a `tracing` subscriber, calls into
//! `pdf-core`, and formats the result. It holds no PDF logic of its own.

use anyhow::Result;
use clap::{Parser, Subcommand};
use pdf_core::Capabilities;

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

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show which optional features this build supports.
    Capabilities,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Capabilities => print_capabilities(),
    }

    Ok(())
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

fn print_capabilities() {
    let caps = Capabilities::current();
    let mark = |enabled: bool| if enabled { "yes" } else { "no" };

    println!("pdf-core   {}", pdf_core::VERSION);
    println!("render     {}", mark(caps.render));
    println!("convert    {}", mark(caps.convert));
    println!("ocr        {}", mark(caps.ocr));
}
