//! PDF/A conversion and conformance checking.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use pdf_core::ops::pdfa::check_pdfa;
use pdf_core::{Document, PdfALevel};

use crate::output::{count, guard_output};

#[derive(Debug, ClapArgs)]
pub struct ToPdfaArgs {
    /// The document to convert.
    input: PathBuf,

    /// Conformance level: 1b, 2b, or 3b.
    #[arg(long, default_value = "2b")]
    level: String,

    /// Where to write the PDF/A document.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn to_pdfa(args: ToPdfaArgs, force: bool, password: Option<&str>) -> Result<()> {
    guard_output(&args.output, force)?;

    let Some(level) = PdfALevel::parse(&args.level) else {
        bail!(
            "invalid --level {:?}; valid levels are 1b, 2b, and 3b",
            args.level
        );
    };

    let mut doc = Document::open_with(&args.input, password)?;
    let report = pdf_core::to_pdfa(&mut doc, level)?;
    doc.save(&args.output)?;

    println!("converted to PDF/A ({})", level.identification());

    let mut notes = Vec::new();
    if report.had_encryption {
        notes.push("encryption removed".to_string());
    }
    if report.javascript_removed > 0 {
        notes.push(format!(
            "{} removed",
            count(report.javascript_removed, "JavaScript action")
        ));
    }
    if report.launch_actions_removed > 0 {
        notes.push(format!(
            "{} removed",
            count(report.launch_actions_removed, "launch action")
        ));
    }
    if report.external_refs_removed > 0 {
        notes.push(format!(
            "{} removed",
            count(report.external_refs_removed, "external reference")
        ));
    }
    if !notes.is_empty() {
        println!("  {}", notes.join(", "));
    }
    if !report.unembedded_fonts.is_empty() {
        println!(
            "  warning: {} not embedded: {}",
            count(report.unembedded_fonts.len(), "font"),
            report.unembedded_fonts.join(", ")
        );
    }
    if !report.conformant {
        println!("  the output still does not pass the structural check:");
        for issue in &report.issues {
            println!("  - {issue}");
        }
    }

    println!("{}", args.output.display());
    Ok(())
}

#[derive(Debug, ClapArgs)]
pub struct CheckPdfaArgs {
    /// The document to check.
    input: PathBuf,
}

pub fn check(args: CheckPdfaArgs, password: Option<&str>) -> Result<()> {
    let doc = Document::open_with(&args.input, password)?;
    let report = check_pdfa(&doc)?;

    if report.conformant {
        println!("{} passes the structural PDF/A check", args.input.display());
        return Ok(());
    }

    for issue in &report.issues {
        println!("{issue}");
    }
    bail!(
        "{} is not PDF/A-conformant: {}",
        args.input.display(),
        count(report.issues.len(), "issue")
    );
}
