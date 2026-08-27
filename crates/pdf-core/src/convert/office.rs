//! Office formats, via headless LibreOffice.
//!
//! LibreOffice is invoked as a subprocess, never linked, so its licence does
//! not reach Foolscap.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{PdfError, Result};
use crate::subprocess::LIBREOFFICE;

/// How long to let a single conversion run. Large presentations are genuinely
/// slow; a wedged LibreOffice is not, and must not hang the caller forever.
const TIMEOUT: Duration = Duration::from_secs(180);

/// Formats LibreOffice can write a PDF out as.
///
/// Every one of these is lossy. A PDF describes marks on a page; a word
/// processor document describes structure, and the structure has to be guessed
/// back. Expect duplicated runs and rebuilt frames.
///
/// Plain text is deliberately absent. LibreOffice's PDF import puts text into
/// frames and its plain-text exporter ignores frame content, so the result is
/// an empty file. Use `extract_text`, which reads the text out of the PDF
/// directly, instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeFormat {
    Docx,
    Odt,
    Rtf,
}

impl OfficeFormat {
    /// The filter name to hand LibreOffice, and the extension it produces.
    pub fn as_str(&self) -> &'static str {
        match self {
            OfficeFormat::Docx => "docx",
            OfficeFormat::Odt => "odt",
            OfficeFormat::Rtf => "rtf",
        }
    }
}

/// Whether LibreOffice is installed and usable.
pub fn is_available() -> bool {
    LIBREOFFICE.is_available()
}

/// Convert any format LibreOffice understands into a PDF in `out_dir`.
///
/// Returns the path written.
pub fn office_to_pdf(input: &Path, out_dir: &Path) -> Result<PathBuf> {
    convert(input, "pdf", out_dir, None)
}

/// Convert a PDF into an office format.
///
/// Lossy by nature: callers should tell the user so.
pub fn pdf_to_office(input: &Path, format: OfficeFormat, out_dir: &Path) -> Result<PathBuf> {
    tracing::warn!(
        format = format.as_str(),
        "PDF to office conversion is approximate; layout and structure will differ"
    );
    // Without an explicit input filter, LibreOffice opens a PDF in Draw, which
    // cannot export any of these formats — it exits zero having written
    // nothing. Naming the Writer PDF importer is what makes the conversion
    // possible at all.
    convert(input, format.as_str(), out_dir, Some("writer_pdf_import"))
}

fn convert(
    input: &Path,
    target: &str,
    out_dir: &Path,
    input_filter: Option<&str>,
) -> Result<PathBuf> {
    if !input.exists() {
        return Err(PdfError::NotFound(input.to_path_buf()));
    }

    std::fs::create_dir_all(out_dir).map_err(|source| PdfError::Io {
        path: out_dir.to_path_buf(),
        source,
    })?;

    // LibreOffice keeps a lock on its user profile. Two concurrent invocations
    // sharing the default profile do not queue — the second silently attaches
    // to the first instance and then hangs or exits without converting
    // anything. An isolated profile per call is the only reliable fix, and it
    // is the single most common way headless LibreOffice integrations break.
    let profile = ProfileDir::new()?;

    let mut arguments = vec![
        "--headless".to_string(),
        "--norestore".to_string(),
        "--invisible".to_string(),
        "--nolockcheck".to_string(),
        format!("-env:UserInstallation={}", profile.as_url()),
    ];

    if let Some(filter) = input_filter {
        arguments.push(format!("--infilter={filter}"));
    }

    arguments.extend([
        "--convert-to".to_string(),
        target.to_string(),
        "--outdir".to_string(),
        out_dir.to_string_lossy().into_owned(),
        input.to_string_lossy().into_owned(),
    ]);

    LIBREOFFICE.run(&arguments, TIMEOUT)?;

    let produced = expected_output(input, target, out_dir);

    if !produced.exists() {
        // LibreOffice exits zero even when it has silently refused to convert
        // something, so the absence of the file is the real error signal.
        return Err(PdfError::ToolFailed {
            tool: LIBREOFFICE.name,
            status: None,
            message: format!(
                "reported success but produced no {} file for {}",
                target,
                input.display()
            ),
        });
    }

    Ok(produced)
}

fn expected_output(input: &Path, target: &str, out_dir: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());

    out_dir.join(format!("{stem}.{target}"))
}

/// A throwaway LibreOffice user profile, removed when the conversion is done.
struct ProfileDir {
    path: PathBuf,
}

impl ProfileDir {
    fn new() -> Result<Self> {
        // A counter rather than a clock or a random number: this has to be
        // unique among concurrent calls in one process, and the process id
        // separates it from other processes.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let path = std::env::temp_dir().join(format!(
            "foolscap-soffice-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));

        std::fs::create_dir_all(&path).map_err(|source| PdfError::Io {
            path: path.clone(),
            source,
        })?;

        Ok(Self { path })
    }

    /// LibreOffice wants the profile as a file URL, not a path.
    fn as_url(&self) -> String {
        format!("file://{}", self.path.display())
    }
}

impl Drop for ProfileDir {
    fn drop(&mut self) {
        // Best effort: a leftover profile directory in the temporary directory
        // is untidy, not harmful, and there is nothing useful to do on failure.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_map_to_their_extensions() {
        assert_eq!(OfficeFormat::Docx.as_str(), "docx");
        assert_eq!(OfficeFormat::Rtf.as_str(), "rtf");
    }

    #[test]
    fn the_output_name_follows_the_input_stem() {
        let out = expected_output(Path::new("/tmp/Report v2.docx"), "pdf", Path::new("/out"));
        assert_eq!(out, Path::new("/out/Report v2.pdf"));
    }

    #[test]
    fn a_pathless_input_still_produces_a_name() {
        let out = expected_output(Path::new("/"), "pdf", Path::new("/out"));
        assert_eq!(out, Path::new("/out/output.pdf"));
    }

    #[test]
    fn each_profile_directory_is_distinct() {
        let first = ProfileDir::new().unwrap();
        let second = ProfileDir::new().unwrap();

        assert_ne!(first.path, second.path);
        assert!(first.path.exists() && second.path.exists());
        assert!(first.as_url().starts_with("file:///"));
    }

    #[test]
    fn a_profile_directory_cleans_itself_up() {
        let path = {
            let profile = ProfileDir::new().unwrap();
            profile.path.clone()
        };

        assert!(!path.exists(), "the profile should have been removed");
    }

    #[test]
    fn converting_a_missing_file_says_so() {
        let error = office_to_pdf(
            Path::new("/definitely/not/here.docx"),
            &std::env::temp_dir(),
        )
        .unwrap_err();

        assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
    }
}
