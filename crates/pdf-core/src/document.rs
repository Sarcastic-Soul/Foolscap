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
///
/// An encrypted file is decrypted as it is opened, so a `Document` always
/// holds plain content, and saving it writes an unencrypted file.
pub struct Document {
    pub(crate) inner: lopdf::Document,
    pub(crate) source: Option<PathBuf>,
    pub(crate) was_encrypted: bool,
}

impl Document {
    /// Open a PDF from disk.
    ///
    /// Many encrypted files have an empty user password: they restrict what
    /// may be done with them but open in every viewer without asking. Those
    /// open here too. A file that really needs a password is rejected with
    /// [`PdfError::Encrypted`]; use [`Document::open_with_password`] for it.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, None)
    }

    /// Open an encrypted PDF with its user or owner password.
    ///
    /// A wrong password is [`PdfError::WrongPassword`]. The password is not
    /// needed, and not checked, for a file that opens without one, so it is
    /// safe to pass the same password for every file in a batch.
    pub fn open_with_password(path: impl AsRef<Path>, password: &str) -> Result<Self> {
        Self::open_with(path, Some(password))
    }

    /// Open a PDF, using `password` only if the file needs one.
    ///
    /// The form to call when a password may or may not have been given, such
    /// as from a command-line option.
    pub fn open_with(path: impl AsRef<Path>, password: Option<&str>) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(PdfError::NotFound(path.to_path_buf()));
        }

        let bytes = std::fs::read(path).map_err(|source| PdfError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        // Loading already tries the empty password, and decrypts the document
        // in place when that works.
        let mut inner =
            lopdf::Document::load_mem(&bytes).map_err(|source| map_load_error(path, source))?;

        // When the empty password does not work, lopdf still returns a
        // document: one holding the encryption dictionary and none of the
        // content. The trailer entry is the dependable sign that nothing was
        // decrypted.
        if is_locked(&inner) {
            let Some(password) = password else {
                return Err(PdfError::Encrypted(path.to_path_buf()));
            };

            // Check the password against the dictionary already in hand before
            // parsing the file a second time. This also separates a wrong
            // password from a scheme that cannot be read at all, which the
            // loader reports identically.
            inner
                .authenticate_password(password)
                .map_err(|source| map_password_error(path, source))?;

            let options = lopdf::LoadOptions::with_password(password);
            inner = lopdf::Document::load_mem_with_options(&bytes, options)
                .map_err(|source| map_load_error(path, source))?;

            if is_locked(&inner) {
                return Err(PdfError::WrongPassword(path.to_path_buf()));
            }
        }

        // lopdf remembers how the file was encrypted so that an incremental
        // update can be encrypted to match. Foolscap always writes whole files,
        // and they are meant to come out decrypted, so forget it.
        let was_encrypted = inner.encryption_state.take().is_some();

        Ok(Self {
            inner,
            source: Some(path.to_path_buf()),
            was_encrypted,
        })
    }

    /// Wrap an already-built `lopdf` document.
    pub(crate) fn from_lopdf(inner: lopdf::Document, source: Option<PathBuf>) -> Self {
        Self {
            inner,
            source,
            was_encrypted: false,
        }
    }

    /// Whether the file this was opened from was encrypted.
    ///
    /// The document itself is decrypted either way; this says whether saving
    /// it removes protection the original had.
    pub fn was_encrypted(&self) -> bool {
        self.was_encrypted
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

    /// Encrypt the document in place and write it to `path`.
    ///
    /// Overwrites unconditionally; the caller is responsible for asking the
    /// user first. See [`crate::ops::encrypt`] for the scheme written.
    pub fn save_encrypted(
        &mut self,
        path: impl AsRef<Path>,
        opts: &crate::ops::EncryptOptions,
    ) -> Result<()> {
        crate::ops::encrypt(self, opts)?;
        self.save(path)
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

/// Whether lopdf left the document encrypted, having found no password that
/// opens it.
fn is_locked(doc: &lopdf::Document) -> bool {
    doc.trailer.get(b"Encrypt").is_ok()
}

/// Translate a load failure into something a user can act on.
fn map_load_error(path: &Path, source: lopdf::Error) -> PdfError {
    match source {
        lopdf::Error::IO(source) => PdfError::Io {
            path: path.to_path_buf(),
            source,
        },
        lopdf::Error::InvalidPassword
        | lopdf::Error::Decryption(_)
        | lopdf::Error::UnsupportedSecurityHandler(_) => map_password_error(path, source),
        other => PdfError::Malformed {
            path: path.to_path_buf(),
            reason: other.to_string(),
        },
    }
}

/// Translate a failure to unlock a document. Only an incorrect password is
/// something the user can fix by typing something else; anything else is a
/// scheme that cannot be read.
fn map_password_error(path: &Path, source: lopdf::Error) -> PdfError {
    use lopdf::encryption::DecryptionError;

    match source {
        lopdf::Error::InvalidPassword
        | lopdf::Error::Decryption(DecryptionError::IncorrectPassword) => {
            PdfError::WrongPassword(path.to_path_buf())
        }
        lopdf::Error::UnsupportedSecurityHandler(name) => PdfError::UnsupportedEncryption {
            path: path.to_path_buf(),
            reason: format!(
                "the {} security handler is not supported",
                String::from_utf8_lossy(&name)
            ),
        },
        other => PdfError::UnsupportedEncryption {
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
