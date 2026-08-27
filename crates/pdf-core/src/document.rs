//! The owned document handle every operation takes.

use std::path::{Path, PathBuf};

use lopdf::{Object, StringFormat};

use crate::error::{PdfError, Result};
use crate::text;

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

impl Metadata {
    /// True when every field is unset.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.author.is_none()
            && self.subject.is_none()
            && self.keywords.is_none()
            && self.creator.is_none()
            && self.producer.is_none()
    }

    /// The fields, paired with the PDF Info dictionary keys they map to.
    fn fields(&self) -> [(&'static [u8], &Option<String>); 6] {
        [
            (b"Title", &self.title),
            (b"Author", &self.author),
            (b"Subject", &self.subject),
            (b"Keywords", &self.keywords),
            (b"Creator", &self.creator),
            (b"Producer", &self.producer),
        ]
    }
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
    /// Encrypted documents are rejected with [`PdfError::Encrypted`] rather
    /// than surfacing later as confusing parse failures: `lopdf` loads the
    /// object structure of an encrypted file successfully, but every string and
    /// stream in it is ciphertext until a password is supplied.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(PdfError::NotFound(path.to_path_buf()));
        }

        let inner = lopdf::Document::load(path).map_err(|source| map_load_error(path, source))?;

        if inner.is_encrypted() {
            return Err(PdfError::Encrypted(path.to_path_buf()));
        }

        Ok(Self {
            inner,
            source: Some(path.to_path_buf()),
        })
    }

    /// Wrap an already-built `lopdf` document.
    pub(crate) fn from_lopdf(inner: lopdf::Document, source: Option<PathBuf>) -> Self {
        Self { inner, source }
    }

    /// Write the document to `path`.
    ///
    /// Overwrites unconditionally; the caller is responsible for asking the
    /// user first.
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|source| PdfError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }

        self.inner.save(path).map_err(|source| PdfError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(())
    }

    /// Number of pages in the document.
    pub fn page_count(&self) -> usize {
        self.inner.get_pages().len()
    }

    /// The path this document was loaded from, if any.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// The PDF specification version the document declares.
    pub fn version(&self) -> &str {
        &self.inner.version
    }

    /// Read the document information dictionary.
    ///
    /// Missing fields come back as `None`. Text is decoded from either
    /// UTF-16BE (the form producers use for non-ASCII text) or PDFDocEncoding.
    pub fn metadata(&self) -> Result<Metadata> {
        let Some(info) = self.info_dictionary() else {
            return Ok(Metadata::default());
        };

        let read = |key: &[u8]| -> Option<String> {
            let bytes = match info.get(key).ok()? {
                Object::String(bytes, _) => bytes,
                _ => return None,
            };
            let decoded = text::decode_pdf_string(bytes);
            (!decoded.is_empty()).then_some(decoded)
        };

        Ok(Metadata {
            title: read(b"Title"),
            author: read(b"Author"),
            subject: read(b"Subject"),
            keywords: read(b"Keywords"),
            creator: read(b"Creator"),
            producer: read(b"Producer"),
        })
    }

    /// Replace the document information dictionary.
    ///
    /// Fields set to `None` are removed from the dictionary rather than written
    /// as empty strings.
    pub fn set_metadata(&mut self, metadata: &Metadata) -> Result<()> {
        let info_id = match self.info_dictionary_id() {
            Some(id) => id,
            None => {
                let id = self.inner.add_object(lopdf::Dictionary::new());
                self.inner.trailer.set("Info", Object::Reference(id));
                id
            }
        };

        let updates: Vec<(&'static [u8], Option<Vec<u8>>)> = metadata
            .fields()
            .iter()
            .map(|(key, value)| {
                (
                    *key,
                    value.as_ref().map(|text| text::encode_pdf_string(text)),
                )
            })
            .collect();

        let info = self.inner.get_object_mut(info_id)?.as_dict_mut()?;

        for (key, value) in updates {
            match value {
                Some(bytes) => info.set(key, Object::String(bytes, StringFormat::Literal)),
                None => {
                    info.remove(key);
                }
            }
        }

        Ok(())
    }

    fn info_dictionary_id(&self) -> Option<lopdf::ObjectId> {
        self.inner
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|object| object.as_reference().ok())
            .filter(|id| self.inner.objects.contains_key(id))
    }

    fn info_dictionary(&self) -> Option<&lopdf::Dictionary> {
        let id = self.info_dictionary_id()?;
        self.inner.get_dictionary(id).ok()
    }
}

/// Translate a load failure into something a user can act on.
fn map_load_error(path: &Path, source: lopdf::Error) -> PdfError {
    match source {
        lopdf::Error::IO(source) => PdfError::Io {
            path: path.to_path_buf(),
            source,
        },
        lopdf::Error::Decryption(_) => PdfError::Encrypted(path.to_path_buf()),
        other => PdfError::Malformed {
            path: path.to_path_buf(),
            reason: other.to_string(),
        },
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
