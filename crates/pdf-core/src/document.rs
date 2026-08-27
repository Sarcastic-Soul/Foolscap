//! The owned document handle every operation takes.

use std::path::{Path, PathBuf};

use crate::error::{PdfError, Result};

/// Document information dictionary fields Foolscap reads and writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
}

/// An open PDF.
///
/// Wraps [`lopdf::Document`] rather than exposing it, so that the backing
/// library stays an implementation detail.
pub struct Document {
    pub(crate) inner: lopdf::Document,
    pub(crate) source: Option<PathBuf>,
}

impl Document {
    /// Open a PDF from disk.
    ///
    /// Stage 1 fills this in, including the `/Encrypt` check that turns
    /// `lopdf`'s opaque failure on password-protected files into
    /// [`PdfError::Encrypted`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(PdfError::NotFound(path.to_path_buf()));
        }
        todo!("stage 1: document loading")
    }

    /// Write the document to `path`.
    pub fn save(&mut self, _path: impl AsRef<Path>) -> Result<()> {
        todo!("stage 1: document saving")
    }

    /// Number of pages in the document.
    pub fn page_count(&self) -> usize {
        self.inner.get_pages().len()
    }

    /// The path this document was loaded from, if any.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Read the document information dictionary.
    pub fn metadata(&self) -> Result<Metadata> {
        todo!("stage 1: metadata read")
    }

    /// Replace the document information dictionary.
    pub fn set_metadata(&mut self, _metadata: &Metadata) -> Result<()> {
        todo!("stage 1: metadata write")
    }
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("source", &self.source)
            .field("pages", &self.page_count())
            .finish()
    }
}
