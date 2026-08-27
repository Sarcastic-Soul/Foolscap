//! Formatting helpers. Everything the user reads is produced here.

use std::path::Path;

use anyhow::{bail, Result};
use pdf_core::Capabilities;

/// Refuse to clobber an existing file unless the user asked for it.
pub fn guard_output(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite",
            path.display()
        );
    }
    Ok(())
}

/// Render a byte count the way a person would say it.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `1 page` / `3 pages`, so messages read correctly.
pub fn pages(count: usize) -> String {
    if count == 1 {
        "1 page".to_string()
    } else {
        format!("{count} pages")
    }
}

/// `1 object` / `4 objects`, and so on for any singular noun.
pub fn count(number: usize, noun: &str) -> String {
    if number == 1 {
        format!("{number} {noun}")
    } else {
        format!("{number} {noun}s")
    }
}

pub fn print_capabilities() {
    let caps = Capabilities::current();
    let mark = |enabled: bool| if enabled { "yes" } else { "no" };

    println!("pdf-core   {}", pdf_core::VERSION);
    println!("render     {}", mark(caps.render));
    println!("convert    {}", mark(caps.convert));
    println!("ocr        {}", mark(caps.ocr));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_counts_stay_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1_572_864), "1.5 MB");
    }

    #[test]
    fn nouns_are_pluralised() {
        assert_eq!(count(1, "object"), "1 object");
        assert_eq!(count(0, "object"), "0 objects");
        assert_eq!(count(3, "object"), "3 objects");
    }

    #[test]
    fn page_counts_are_pluralised() {
        assert_eq!(pages(1), "1 page");
        assert_eq!(pages(0), "0 pages");
        assert_eq!(pages(7), "7 pages");
    }
}
