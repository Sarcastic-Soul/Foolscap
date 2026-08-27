use std::path::PathBuf;

/// Result alias used throughout `pdf-core`.
pub type Result<T> = std::result::Result<T, PdfError>;

/// Every failure `pdf-core` can produce.
///
/// Variants are typed rather than stringly so that callers — the CLI today, the
/// GUI later — can react differently to, say, an encrypted document than to a
/// missing file.
#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("file not found: {0}")]
    NotFound(PathBuf),

    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0} is encrypted; Foolscap cannot open password-protected documents yet")]
    Encrypted(PathBuf),

    #[error("{path} is not a valid PDF: {reason}")]
    Malformed { path: PathBuf, reason: String },

    /// A page number that does not exist in the document. Both values are
    /// one-indexed, matching what the user typed.
    #[error("page {requested} is out of range; document has {total} page(s)")]
    PageOutOfRange { requested: usize, total: usize },

    #[error("invalid page range {spec:?}: {reason}")]
    InvalidPageRange { spec: String, reason: String },

    #[error("refusing to overwrite existing file: {0}")]
    OutputExists(PathBuf),

    #[error("no pages selected")]
    EmptySelection,

    #[error("{0} degrees is not a multiple of 90")]
    InvalidRotation(i32),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("could not render {path}: {reason}")]
    Render { path: PathBuf, reason: String },

    #[error("this build of Foolscap was compiled without the {0:?} feature")]
    FeatureDisabled(&'static str),

    #[error("{tool} is not installed or not on PATH; install it with: {install}")]
    ToolMissing {
        tool: &'static str,
        install: &'static str,
    },

    #[error("{tool} failed{}: {message}", .status.map(|code| format!(" with exit code {code}")).unwrap_or_default())]
    ToolFailed {
        tool: &'static str,
        status: Option<i32>,
        message: String,
    },

    #[error("{tool} did not finish within {seconds} seconds")]
    ToolTimeout { tool: &'static str, seconds: u64 },

    #[error("{0} is not an image Foolscap can read")]
    UnsupportedImage(PathBuf),

    #[error(transparent)]
    Lopdf(#[from] lopdf::Error),
}
