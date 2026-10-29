use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use pdf_core::{verify_signatures, Document};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The document whose signatures to check.
    input: PathBuf,
}

pub fn run(args: Args, password: Option<&str>) -> Result<()> {
    let doc = Document::open_with(&args.input, password)?;
    let reports = verify_signatures(&doc)?;

    if reports.is_empty() {
        println!("no signatures");
        return Ok(());
    }

    for report in &reports {
        println!(
            "signature  {}",
            report.name.as_deref().unwrap_or("(unnamed)")
        );
        if let Some(signer) = &report.signer {
            println!("signer     {signer}");
        }
        if let Some(issuer) = &report.issuer {
            println!("issuer     {issuer}");
        }
        if let Some(signed_at) = &report.signed_at {
            println!("signed at  {signed_at}");
        }
        if let Some(reason) = &report.signing_reason {
            println!("reason     {reason}");
        }
        if let Some(sub_filter) = &report.sub_filter {
            println!("filter     {sub_filter}");
        }
        if report.valid_from.is_some() || report.valid_until.is_some() {
            println!(
                "certificate valid {} .. {}",
                report.valid_from.as_deref().unwrap_or("?"),
                report.valid_until.as_deref().unwrap_or("?")
            );
        }
        println!(
            "verdict    {}",
            if report.intact { "INTACT" } else { "BROKEN" }
        );
        println!(
            "trust      {}",
            match &report.trust {
                Some(pdf_core::Trust::Trusted) => "TRUSTED (chains to a system root)",
                Some(pdf_core::Trust::Untrusted(_)) => "NOT TRUSTED",
                None => "NOT CHECKED",
            }
        );
        println!("           {}", report.reason);
    }

    if reports.iter().any(|report| !report.intact) {
        anyhow::bail!("at least one signature failed verification");
    }

    Ok(())
}
