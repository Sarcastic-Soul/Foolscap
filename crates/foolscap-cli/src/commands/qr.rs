use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use pdf_core::{qr_code, QrEcc, QrOptions};

use crate::output::guard_output;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The text or URL the QR code encodes.
    text: String,

    /// Edge length of the code in millimetres.
    #[arg(long, default_value_t = 50.0)]
    size: f64,

    /// Error correction: low, medium, quartile, or high.
    #[arg(long, default_value = "medium")]
    ecc: String,

    /// Label drawn beneath the code.
    #[arg(long, value_name = "TEXT")]
    label: Option<String>,

    /// Where to write the PDF.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool) -> Result<()> {
    if !(args.size.is_finite() && args.size > 0.0) {
        bail!(
            "--size must be a positive number of millimetres, got {}",
            args.size
        );
    }

    let ecc = parse_ecc(&args.ecc)?;

    let options = QrOptions {
        size_mm: args.size,
        ecc,
        label: args.label,
    };

    let mut doc = qr_code(&args.text, &options)?;

    guard_output(&args.output, force)?;
    doc.save(&args.output)?;
    println!("{}", args.output.display());

    Ok(())
}

/// Parse the `--ecc` value, case-insensitively.
fn parse_ecc(spec: &str) -> Result<QrEcc> {
    match spec.to_ascii_lowercase().as_str() {
        "low" => Ok(QrEcc::Low),
        "medium" => Ok(QrEcc::Medium),
        "quartile" => Ok(QrEcc::Quartile),
        "high" => Ok(QrEcc::High),
        other => bail!("--ecc must be one of low, medium, quartile, or high, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecc_names_parse_case_insensitively() {
        assert_eq!(parse_ecc("low").unwrap(), QrEcc::Low);
        assert_eq!(parse_ecc("MEDIUM").unwrap(), QrEcc::Medium);
        assert_eq!(parse_ecc("Quartile").unwrap(), QrEcc::Quartile);
        assert_eq!(parse_ecc("high").unwrap(), QrEcc::High);
    }

    #[test]
    fn unknown_ecc_names_are_rejected() {
        for bad in ["", "max", "ultra", "medium high"] {
            assert!(parse_ecc(bad).is_err(), "{bad:?} should not parse");
        }
    }
}
