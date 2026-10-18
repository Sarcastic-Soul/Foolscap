use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use crate::output::guard_output;

#[derive(Debug, Parser)]
pub struct Args {
    /// The damaged PDF to repair.
    input: PathBuf,

    /// Where to write the repaired PDF.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let report = pdf_core::repair_with(&args.input, &args.output, password)?;

    if report.is_clean() {
        println!("No repairs needed; {} written.", args.output.display());
    } else {
        println!(
            "Repaired {} -> {}",
            args.input.display(),
            args.output.display()
        );
        println!("  Objects recovered: {}", report.recovered_objects);
        println!("  Objects dropped: {}", report.dropped_objects);
        println!("  Stream lengths fixed: {}", report.fixed_streams);
        for warning in &report.warnings {
            println!("  Warning: {warning}");
        }
    }

    Ok(())
}
