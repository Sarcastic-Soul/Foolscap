//! Writing encrypted output.
//!
//! Opening encrypted documents already works (see [`Document::open_with`]);
//! this module is the other half: protecting a plain document with a user
//! and/or owner password. Only AES-256 (revision 6, the PDF 2.0 scheme) is
//! written — the older RC4 and AES-128 handlers exist for reading other
//! producers' files, not for new ones.

use std::collections::BTreeMap;
use std::sync::Arc;

use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{EncryptionState, EncryptionVersion, Object, StringFormat};

use crate::document::Document;
use crate::error::{PdfError, Result};

/// Re-exported so callers can name the permission set without depending on
/// `lopdf` themselves.
pub use lopdf::Permissions;

/// How to encrypt a document.
#[derive(Debug, Clone)]
pub struct EncryptOptions {
    /// Password needed to open the document. May be empty, in which case the
    /// file opens without asking but owner restrictions still apply.
    pub user_password: String,
    /// Password that grants full rights. Defaults to the user password when
    /// empty at the CLI layer; here it is taken literally.
    pub owner_password: String,
    /// What a user-password opener may do. Defaults to everything.
    pub permissions: Permissions,
    /// Whether the Info dictionary and XMP stream are encrypted too.
    /// Cleartext metadata leaks titles and authors to anyone with the raw
    /// bytes; leave this on unless a workflow needs the metadata visible.
    pub encrypt_metadata: bool,
}

impl Default for EncryptOptions {
    fn default() -> Self {
        Self {
            user_password: String::new(),
            owner_password: String::new(),
            permissions: Permissions::all(),
            encrypt_metadata: true,
        }
    }
}

/// Encrypt `doc` in place with AES-256 (revision 6).
///
/// The document must not already be encrypted — [`Document`] opens files
/// decrypted, so this only fails when `encrypt` is called twice without an
/// intervening save and reopen.
pub fn encrypt(doc: &mut Document, opts: &EncryptOptions) -> Result<()> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // The revision-6 key is random rather than derived, so generate one. Read
    // from the OS pool; the time/process hash is only a fallback for hosts
    // without /dev/urandom.
    let mut file_key = read_urandom(32);
    if file_key.len() != 32 {
        let mut hasher = DefaultHasher::new();
        (
            std::time::SystemTime::now(),
            std::process::id(),
            doc.page_count(),
        )
            .hash(&mut hasher);
        let seed = hasher.finish();
        file_key = (0..32).map(|i| (seed >> (8 * (i % 8))) as u8).collect();
    }

    // The older handlers mix the file identifier into the key, and every
    // producer writes one, so ensure the trailer has an ID before encrypting.
    if doc.inner.trailer.get(b"ID").is_err() {
        let first = Object::String(fresh_id(16), StringFormat::Hexadecimal);
        let second = Object::String(fresh_id(16), StringFormat::Hexadecimal);
        doc.inner
            .trailer
            .set("ID", Object::Array(vec![first, second]));
    }

    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let version = EncryptionVersion::V5 {
        encrypt_metadata: opts.encrypt_metadata,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &file_key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: &opts.owner_password,
        user_password: &opts.user_password,
        permissions: opts.permissions,
    };

    let state =
        EncryptionState::try_from(version).map_err(|source| PdfError::UnsupportedEncryption {
            path: doc.source.clone().unwrap_or_default(),
            reason: source.to_string(),
        })?;

    doc.inner.encrypt(&state).map_err(|source| {
        if matches!(source, lopdf::Error::AlreadyEncrypted) {
            PdfError::AlreadyEncrypted
        } else {
            PdfError::Internal(format!("could not encrypt the document: {source}"))
        }
    })?;

    // lopdf leaves /Length out of an AES-256 dictionary. The standard makes it
    // optional there, but Poppler then assumes a short key and decrypts the
    // file into garbage, so state it as every other AES-256 producer does.
    let encrypt_id = doc
        .inner
        .trailer
        .get(b"Encrypt")
        .and_then(Object::as_reference)
        .map_err(|source| PdfError::Internal(format!("encryption left no /Encrypt: {source}")))?;
    doc.inner
        .get_dictionary_mut(encrypt_id)
        .map_err(|source| {
            PdfError::Internal(format!("the /Encrypt dictionary is missing: {source}"))
        })?
        .set("Length", 256);

    Ok(())
}

/// Read `n` bytes from the OS entropy pool, or an empty vector when the pool
/// cannot be read (the caller falls back to a weaker seed).
fn read_urandom(n: usize) -> Vec<u8> {
    use std::io::Read;

    let mut bytes = vec![0u8; n];
    let Ok(mut file) = std::fs::File::open("/dev/urandom") else {
        return Vec::new();
    };
    if file.read_exact(&mut bytes).is_err() {
        return Vec::new();
    }
    bytes
}

/// `n` fresh random bytes for a trailer ID half.
pub(crate) fn fresh_id(n: usize) -> Vec<u8> {
    let bytes = read_urandom(n);
    if bytes.len() == n {
        return bytes;
    }
    // Fallback: not cryptographic, but an ID only needs uniqueness, and this
    // path only runs where /dev/urandom is absent.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut out = Vec::with_capacity(n);
    let mut counter = 0u64;
    while out.len() < n {
        let mut hasher = DefaultHasher::new();
        (std::time::SystemTime::now(), std::process::id(), counter).hash(&mut hasher);
        out.extend_from_slice(&hasher.finish().to_le_bytes());
        counter += 1;
    }
    out.truncate(n);
    out
}
