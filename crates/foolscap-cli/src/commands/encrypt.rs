use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;
use pdf_core::{Document, EncryptOptions};

use crate::output::{guard_output, pages};

#[derive(Debug, Parser)]
pub struct Args {
    /// The document to protect.
    input: PathBuf,

    /// Password needed to open the result. May be omitted when only an owner
    /// password is given; the file then opens without asking but stays
    /// restricted.
    #[arg(long, value_name = "PASSWORD")]
    user_password: Option<String>,

    /// Password that grants full rights. Defaults to the user password.
    #[arg(long, value_name = "PASSWORD")]
    owner_password: Option<String>,

    /// Leave the Info dictionary and XMP packet readable without the password.
    /// On by default they are encrypted too.
    #[arg(long)]
    expose_metadata: bool,

    /// Where to write the encrypted copy.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args, force: bool, password: Option<&str>) -> Result<()> {
    let user_password = args.user_password.unwrap_or_default();
    let owner_password = args.owner_password.unwrap_or_else(|| user_password.clone());
    if user_password.is_empty() && owner_password.is_empty() {
        bail!("nothing to encrypt with; pass --user-password and/or --owner-password");
    }

    guard_output(&args.output, force)?;

    let mut doc = Document::open_with(&args.input, password)?;
    doc.save_encrypted(
        &args.output,
        &EncryptOptions {
            user_password,
            owner_password,
            encrypt_metadata: !args.expose_metadata,
            ..EncryptOptions::default()
        },
    )?;

    // Passwords are never printed or logged.
    println!(
        "{} -> {} ({}, now encrypted with AES-256)",
        args.input.display(),
        args.output.display(),
        pages(doc.page_count())
    );

    Ok(())
}
