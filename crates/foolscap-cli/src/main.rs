//! The `foolscap` binary. Argument parsing lives in the library beside it.

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    foolscap_cli::run(foolscap_cli::Cli::parse())
}
