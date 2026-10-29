//! Digital signature verification: parse, check integrity, read the signer's
//! certificate, and verify the signature cryptographically where the key
//! algorithm is supported.
//!
//! Entry point: the catalog's `/AcroForm /Fields` array and every page's
//! `/Annots` are walked for `/FT /Sig` widgets carrying a `/V` signature
//! dictionary. Each `/V` holds a `/ByteRange` naming the two spans of the
//! original file the signer committed to (everything but the zero-padded
//! `/Contents` hole) and the `/Contents` DER CMS `SignedData` object.
//!
//! For each signature:
//!
//! 1. The digest is recomputed over the current file bytes named by
//!    `/ByteRange` and compared with the `messageDigest` authenticated
//!    attribute, or the encapsulated digest in `eContent` for the
//!    `adbe.pkcs7.sha1` packaging that uses one. (That sub-filter's ISO
//!    wording about hashing `/Contents` itself is self-referential — the
//!    digest would cover the bytes that carry it — and RFC 3778 §5 confirms
//!    the digest covers the byte range.) This is the tamper check.
//! 2. The signer's certificate is parsed for subject, issuer and validity
//!    window, and selected by the `SignerInfo` issuer-and-serial identifier.
//! 3. The signature is verified where possible — over the DER of the
//!    authenticated attributes when present, else of the signed content —
//!    with RSA PKCS#1 v1.5 through the `rsa` crate and ECDSA P-256 through
//!    `p256`. Other key algorithms (ECDSA P-384 and friends) leave `intact`
//!    resting on the digest match alone, and the reason says the algorithm
//!    was not verified.
//!
//! Chain-of-trust: the signer's certificate is followed through the certificates
//! carried in the CMS and then matched against a set of trusted root
//! certificates, by default the system CA bundle. Each link is checked with
//! the issuer's public key. Validity periods are not checked, so a report can
//! say a chain reaches a trusted root while the certificate has since expired.
//! Revocation (CRL, OCSP) is not checked either.
//!
//! Every report also says whether the chain reached a trusted root. Integrity
//! (`intact`) and trust are separate: a valid signature from an unknown signer
//! is intact but untrusted.
//!
//! Everything reportable is reported rather than raised: a document with no
//! signature fields yields an empty `Vec`, and an unreadable `/V` yields one
//! report with `intact = false` and the parse failure in its reason. Only
//! failures to re-read the source file itself are errors.

use std::collections::BTreeSet;

use lopdf::{Dictionary, Object, ObjectId};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};
use x509_parser::certificate::X509Certificate;
use x509_parser::der_parser::asn1_rs::{Any, Class, FromDer, Tag};
use x509_parser::public_key::PublicKey;
use x509_parser::x509::X509Name;

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::text;

/// How deeply to follow `/Kids` before deciding a form is malformed.
const MAX_DEPTH: usize = 32;

/// The note every report carries when the chain was not reached.
const NO_CHAIN_NOTE: &str = "certificate chain-of-trust was NOT checked";

/// How many certificates to follow from the signer to a root.
const MAX_CHAIN_DEPTH: usize = 10;

/// The system trust store, a PEM bundle of root certificates.
const SYSTEM_TRUST_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The result of DER walking: a failure message for the report, never a
/// `PdfError`, because an unparseable signature is a report, not an error.
type Parse<T> = std::result::Result<T, String>;

// ---------------------------------------------------------------- the report

/// The outcome for one signature field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SignatureReport {
    /// The field name, when the dictionary carries `/T`.
    pub name: Option<String>,
    /// The signer: the certificate's subject common name when the CMS was
    /// parsed, else the signature dictionary's `/Name`.
    pub signer: Option<String>,
    /// The certificate issuer's common name, when a certificate was parsed.
    pub issuer: Option<String>,
    /// The certificate's not-before time, formatted `YYYY-MM-DD HH:MM:SS UTC`.
    pub valid_from: Option<String>,
    /// The certificate's not-after time, same format.
    pub valid_until: Option<String>,
    /// The signing time claimed by `/M`, when present and parseable.
    pub signed_at: Option<String>,
    /// The sub-filter naming the signature encoding, e.g.
    /// `adbe.pkcs7.detached`.
    pub sub_filter: Option<String>,
    /// The signer's stated reason, `/Reason`.
    pub signing_reason: Option<String>,
    /// Whether the signed bytes still hash to the signed digest and the
    /// signature held (or no supported algorithm could check it, in which
    /// case the digest match alone carries the verdict).
    pub intact: bool,
    /// Whether the signer's certificate chains to a trusted root. `None`
    /// when the chain was never reached, because an earlier check failed.
    pub trust: Option<Trust>,
    /// Human-readable detail: what was checked, what mismatched, and the
    /// chain-of-trust result.
    pub reason: String,
}

/// Whether a signer's certificate chains to a trusted root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Every link verifies and the chain ends at a trusted root.
    Trusted,
    /// The chain does not reach a trusted root. The string says where it stopped.
    Untrusted(String),
}

/// Inspect every signature in the document, checking chains against the
/// system CA bundle.
///
/// A document with no signature fields is not an error: the result is empty.
pub fn verify_signatures(doc: &Document) -> Result<Vec<SignatureReport>> {
    verify_signatures_against(doc, &system_trust_roots())
}

/// The DER of every certificate in the system CA bundle. A missing or
/// unreadable bundle yields none, and every chain then reports as untrusted.
pub fn system_trust_roots() -> Vec<Vec<u8>> {
    match std::fs::read(SYSTEM_TRUST_BUNDLE) {
        Ok(pem) => x509_parser::pem::Pem::iter_from_buffer(&pem)
            .filter_map(|block| block.ok())
            .map(|block| block.contents)
            .collect(),
        Err(reason) => {
            tracing::warn!("cannot read {SYSTEM_TRUST_BUNDLE}: {reason}");
            Vec::new()
        }
    }
}

/// [`verify_signatures`] with the trusted roots given as DER certificates.
pub fn verify_signatures_against(
    doc: &Document,
    trusted_roots_der: &[Vec<u8>],
) -> Result<Vec<SignatureReport>> {
    let fields = collect_fields(&doc.inner);
    if fields.is_empty() {
        return Ok(Vec::new());
    }

    let roots: Vec<X509Certificate<'_>> = trusted_roots_der
        .iter()
        .filter_map(|der| {
            x509_parser::parse_x509_certificate(der)
                .ok()
                .map(|(_, cert)| cert)
        })
        .collect();

    // Byte-range offsets refer to the file as signed, so the original bytes
    // are needed, not the parsed objects: an incremental update or a spliced
    // hole is invisible in `lopdf`'s view but obvious in the raw file.
    let Some(path) = doc.source() else {
        tracing::warn!("verifying signatures on a document with no source file");
        return Ok(fields
            .into_iter()
            .map(|field| SignatureReport {
                name: field.name,
                intact: false,
                reason: format!(
                    "the document was not opened from a file, so the signed bytes cannot be re-read; {NO_CHAIN_NOTE}"
                ),
                ..SignatureReport::default()
            })
            .collect());
    };

    let bytes = std::fs::read(path).map_err(|source| PdfError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(fields
        .into_iter()
        .map(|field| verify_one(&bytes, field, &roots))
        .collect())
}

// ------------------------------------------------------------ finding fields

/// One signature field worth a report: the `/V` dictionary's entries, read
/// during the walk so the rest of verification works on plain data.
struct Field {
    /// The field name `/T`, when present.
    name: Option<String>,
    /// `/SubFilter`, naming how the digest and signature are packaged.
    sub_filter: Option<String>,
    /// `/Name`, the signer's name as the producer wrote it.
    signer_name: Option<String>,
    /// `/M`, the claimed signing time.
    signed_at: Option<String>,
    /// `/Reason`, why the document was signed.
    signing_reason: Option<String>,
    /// `/ByteRange [o1 l1 o2 l2]`, when present.
    byte_range: Option<Vec<i64>>,
    /// `/Contents`, the DER CMS `SignedData` in its zero-padded hole.
    contents: Option<Vec<u8>>,
    /// Why the signature dictionary could not be read at all, when it could
    /// not; the report is then just this reason.
    failure: Option<String>,
}

impl Field {
    /// A report pre-filled from the dictionary, for the failure paths that
    /// never reach the CMS.
    fn report(&self) -> SignatureReport {
        SignatureReport {
            name: self.name.clone(),
            signer: self.signer_name.clone(),
            signed_at: self.signed_at.clone(),
            sub_filter: self.sub_filter.clone(),
            signing_reason: self.signing_reason.clone(),
            ..SignatureReport::default()
        }
    }
}

/// Every signature field in the document: the AcroForm's `/Fields` tree, then
/// the pages' `/Annots` for widget annotations an orphan producer left out of
/// it. Deduplicated by object id, so a field listed in both is reported once.
fn collect_fields(doc: &lopdf::Document) -> Vec<Field> {
    let mut seen = BTreeSet::new();
    let mut fields = Vec::new();

    if let Some(root) = root_dictionary(doc) {
        if let Some((_, acro)) = root
            .get(b"AcroForm")
            .ok()
            .and_then(|form| resolve(doc, form))
        {
            if let Some(entries) = array_entry(doc, acro, b"Fields") {
                walk_field_list(doc, entries, 0, &mut seen, &mut fields);
            }
        }
    }

    for page_id in doc.get_pages().values() {
        let Ok(page) = doc.get_dictionary(*page_id) else {
            continue;
        };
        let Some(annots) = array_entry(doc, page, b"Annots") else {
            continue;
        };
        for annot in annots {
            let Some((id, dict)) = resolve(doc, annot) else {
                continue;
            };
            if is_signature_field(doc, dict) && id.is_none_or(|id| seen.insert(id)) {
                push_field(doc, dict, &mut fields);
            }
        }
    }

    fields
}

/// Walk a `/Fields` array (or a field's `/Kids`), collecting signature fields.
fn walk_field_list(
    doc: &lopdf::Document,
    entries: &[Object],
    depth: usize,
    seen: &mut BTreeSet<ObjectId>,
    fields: &mut Vec<Field>,
) {
    if depth > MAX_DEPTH {
        tracing::warn!("form field nesting exceeds {MAX_DEPTH} levels; the rest is ignored");
        return;
    }

    for entry in entries {
        let Some((id, dict)) = resolve(doc, entry) else {
            continue;
        };

        if is_signature_field(doc, dict) && id.is_none_or(|id| seen.insert(id)) {
            push_field(doc, dict, fields);
        }

        // A signature field's kids are its widget annotations; `/V` may sit
        // on the field or on a merged kid.
        if let Some(kids) = array_entry(doc, dict, b"Kids") {
            walk_field_list(doc, kids, depth + 1, seen, fields);
        }
    }
}

/// Whether a field/widget dictionary is a signature worth reporting: it is a
/// `/Sig` field, or it carries a `/V` that resolves to a signature dictionary.
fn is_signature_field(doc: &lopdf::Document, dict: &Dictionary) -> bool {
    if dict
        .get(b"FT")
        .ok()
        .and_then(|ft| ft.as_name().ok())
        .is_some_and(|ft| ft == b"Sig")
    {
        return true;
    }
    match dict.get(b"V") {
        Ok(value) => signature_dict(doc, value).is_some_and(|(_, value)| {
            value.get(b"Contents").is_ok() || value.get(b"ByteRange").is_ok()
        }),
        Err(_) => false,
    }
}

/// Read one signature field's `/V` into a [`Field`].
fn push_field(doc: &lopdf::Document, dict: &Dictionary, fields: &mut Vec<Field>) {
    let mut field = Field {
        name: string_entry(dict, b"T"),
        ..unverified("the field has no /V; it was never signed")
    };

    let Some(value) = dict.get(b"V").ok() else {
        fields.push(field);
        return;
    };

    let Some((_, signature)) = signature_dict(doc, value) else {
        field.failure = Some("/V is not a signature dictionary".into());
        fields.push(field);
        return;
    };

    field.sub_filter = name_entry(signature, b"SubFilter");
    field.signer_name = string_entry(signature, b"Name");
    field.signed_at = string_entry(signature, b"M")
        .as_deref()
        .and_then(format_pdf_date);
    field.signing_reason = string_entry(signature, b"Reason");
    field.byte_range = signature
        .get(b"ByteRange")
        .ok()
        .and_then(|range| range.as_array().ok())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_i64().ok())
                .collect()
        });
    field.contents = match signature.get(b"Contents") {
        Ok(Object::String(bytes, _)) => Some(bytes.clone()),
        _ => None,
    };
    field.failure = None;
    fields.push(field);
}

/// The placeholder for a field whose dictionary stopped making sense.
fn unverified(reason: &str) -> Field {
    Field {
        name: None,
        sub_filter: None,
        signer_name: None,
        signed_at: None,
        signing_reason: None,
        byte_range: None,
        contents: None,
        failure: Some(reason.to_string()),
    }
}

/// Resolve a `/V` value to its signature dictionary: a plain dictionary, or
/// the dictionary of the stream object a PDF 2.0 producer may use.
fn signature_dict<'a>(
    doc: &'a lopdf::Document,
    value: &'a Object,
) -> Option<(Option<ObjectId>, &'a Dictionary)> {
    let (id, resolved) = doc.dereference(value).ok()?;
    match resolved {
        Object::Dictionary(dict) => Some((id, dict)),
        Object::Stream(stream) => Some((id, &stream.dict)),
        _ => None,
    }
}

/// The catalog dictionary, when the trailer names one.
fn root_dictionary(doc: &lopdf::Document) -> Option<&Dictionary> {
    let root = doc.trailer.get(b"Root").ok()?;
    resolve(doc, root).map(|(_, dict)| dict)
}

/// Resolve an object through its reference to a dictionary.
fn resolve<'a>(
    doc: &'a lopdf::Document,
    object: &'a Object,
) -> Option<(Option<ObjectId>, &'a Dictionary)> {
    let (id, resolved) = doc.dereference(object).ok()?;
    resolved.as_dict().ok().map(|dict| (id, dict))
}

/// An entry read as an array of objects, dereferenced through any reference.
fn array_entry<'a>(
    doc: &'a lopdf::Document,
    dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a [Object]> {
    let value = dict.get(key).ok()?;
    let (_, resolved) = doc.dereference(value).ok()?;
    resolved.as_array().ok().map(Vec::as_slice)
}

/// A `/T`-style entry: a string decoded from PDF text encodings.
fn string_entry(dict: &Dictionary, key: &[u8]) -> Option<String> {
    match dict.get(key).ok()? {
        Object::String(bytes, _) => {
            let decoded = text::decode_pdf_string(bytes);
            (!decoded.is_empty()).then_some(decoded)
        }
        _ => None,
    }
}

/// A name entry such as `/SubFilter`, as text.
fn name_entry(dict: &Dictionary, key: &[u8]) -> Option<String> {
    dict.get(key)
        .ok()
        .and_then(|name| name.as_name().ok())
        .map(|name| String::from_utf8_lossy(name).into_owned())
}

// ------------------------------------------------------------- verification

/// Verify one field against the file it was signed over.
fn verify_one(bytes: &[u8], field: Field, roots: &[X509Certificate<'_>]) -> SignatureReport {
    let mut report = field.report();

    if let Some(failure) = &field.failure {
        report.intact = false;
        report.reason = format!("{failure}; {NO_CHAIN_NOTE}");
        return report;
    }

    let range_failure = |report: &mut SignatureReport, what: String| {
        report.intact = false;
        report.reason =
            format!("{what}, so the signed bytes cannot be identified; {NO_CHAIN_NOTE}");
    };

    let Some(range) = ByteRange::parse(field.byte_range.as_deref(), bytes.len()) else {
        let what = match field.byte_range.as_deref() {
            None => "the signature dictionary carries no readable /ByteRange".to_string(),
            Some(values) if values.len() != 4 => {
                format!("the /ByteRange has {} entries, not 4", values.len())
            }
            Some(_) => "the /ByteRange points outside the file".to_string(),
        };
        range_failure(&mut report, what);
        return report;
    };

    let Some(contents) = field.contents.filter(|contents| !contents.is_empty()) else {
        range_failure(
            &mut report,
            "the signature dictionary carries no /Contents".into(),
        );
        return report;
    };

    let cms = match Cms::parse(&contents) {
        Ok(cms) => cms,
        Err(reason) => {
            report.intact = false;
            report.reason = format!("could not parse the CMS signature: {reason}; {NO_CHAIN_NOTE}");
            return report;
        }
    };

    let Some(signer) = cms.signers.first() else {
        report.intact = false;
        report.reason = format!("the CMS carries no SignerInfo; {NO_CHAIN_NOTE}");
        return report;
    };

    let Some(digest_alg) = DigestAlg::from_oid(&signer.digest_alg) else {
        report.intact = false;
        report.reason = format!(
            "digest algorithm {} is not supported; {NO_CHAIN_NOTE}",
            signer.digest_alg
        );
        return report;
    };

    // The digest covers the byte ranges for every sub-filter: the sha1/sha256
    // packaging encapsulates the digest in eContent rather than widening the
    // hashed content (see the module docs).
    let digest = digest_alg.digest_ranges(&range, bytes);

    // Digest check: authenticated messageDigest attribute, else the
    // encapsulated digest, else deferred to the signature itself.
    let digest_check = match &signer.signed_attrs {
        Some(attrs) => match attrs.message_digest {
            Some(signed) => Some((
                signed == digest,
                format!(
                    "byte ranges hash to the signed {} digest",
                    digest_alg.name()
                ),
            )),
            None => {
                report.intact = false;
                report.reason =
                    format!("signed attributes carry no messageDigest; {NO_CHAIN_NOTE}");
                return report;
            }
        },
        None => cms.econtent.map(|encapsulated| {
            (
                encapsulated_covers(encapsulated, &digest),
                format!(
                    "the encapsulated {} digest covers the byte ranges",
                    digest_alg.name()
                ),
            )
        }),
    };

    if let Some((false, _)) = digest_check {
        report.intact = false;
        report.reason = format!(
            "the signed bytes hash to a different {0} digest than the signed one — \
             the file was modified after signing; {NO_CHAIN_NOTE}",
            digest_alg.name()
        );
        return report;
    }

    let certificate = cms.signer_certificate(signer);
    if let Some((cert, _)) = certificate {
        fill_certificate_details(&mut report, cert);
    }

    // What the signature covers: the DER of the authenticated attributes with
    // the [0] tag replaced by universal SET, or the signed content itself
    // when there are none — whose digest is the one already computed.
    let signature_digest = match &signer.signed_attrs {
        Some(attrs) => digest_alg.digest_bytes(&attrs.signed_der()),
        None => digest,
    };

    let signature_outcome = match certificate {
        None => SignatureOutcome::NotChecked("no certificate in the CMS".to_string()),
        Some((cert, _)) => verify_signature(cert, signer, &signature_digest, digest_alg),
    };

    let digest_checked = digest_check.is_some();
    let trust = match certificate {
        Some((cert, _)) => {
            let carried: Vec<&X509Certificate<'_>> =
                cms.certs.iter().map(|(carried, _)| carried).collect();
            chain_trust(cert, &carried, roots)
        }
        None => Trust::Untrusted("the CMS carries no certificate for the signer".into()),
    };
    let mut notes = Vec::new();
    match digest_check {
        Some((true, note)) => notes.push(note),
        None => notes.push(
            "no signed attributes or encapsulated digest; the digest is checked by the signature alone"
                .to_string(),
        ),
        Some((false, _)) => unreachable!("a digest mismatch returns above"),
    }

    match &signature_outcome {
        SignatureOutcome::Verified(detail) => notes.push(format!("signature verified ({detail})")),
        SignatureOutcome::Failed(detail) => {
            notes.push(format!("signature does NOT verify ({detail})"))
        }
        SignatureOutcome::NotChecked(detail) => {
            notes.push(format!("algorithm not verified ({detail})"))
        }
    }

    report.intact = match &signature_outcome {
        SignatureOutcome::Verified(_) => true,
        SignatureOutcome::Failed(_) => false,
        // Unsupported key or missing certificate: the verdict rests on the
        // digest alone, which is only meaningful when one was checked.
        SignatureOutcome::NotChecked(_) => digest_checked,
    };

    notes.push(match &trust {
        Trust::Trusted => "certificate chain reaches a trusted root".to_string(),
        Trust::Untrusted(why) => format!("certificate chain NOT trusted: {why}"),
    });
    report.trust = Some(trust);
    report.reason = notes.join("; ");
    report
}

/// Follow the signer's certificate up to a trusted root.
///
/// Issuers are looked for among the trusted roots and the certificates the
/// CMS carried. Each link must verify under its issuer's key. The walk stops
/// at the first root it reaches, or fails at the first link no issuer covers.
fn chain_trust(
    signer: &X509Certificate<'_>,
    carried: &[&X509Certificate<'_>],
    roots: &[X509Certificate<'_>],
) -> Trust {
    let same = |a: &X509Certificate<'_>, b: &X509Certificate<'_>| {
        a.tbs_certificate.as_ref() == b.tbs_certificate.as_ref()
    };
    let is_root = |cert: &X509Certificate<'_>| roots.iter().any(|root| same(root, cert));

    let mut current = signer;
    for _ in 0..MAX_CHAIN_DEPTH {
        if is_root(current) {
            return Trust::Trusted;
        }

        let issuer = roots
            .iter()
            .chain(carried.iter().copied())
            .find(|candidate| {
                candidate.subject() == current.issuer()
                    && !same(candidate, current)
                    && current
                        .verify_signature(Some(candidate.public_key()))
                        .is_ok()
            });

        match issuer {
            None => {
                return Trust::Untrusted(format!(
                    "{} is not issued by any trusted root certificate",
                    common_name(current.subject()).unwrap_or_else(|| "the signer".into())
                ))
            }
            // Reaching a root that issued the current link ends the walk.
            Some(issuer) if is_root(issuer) => return Trust::Trusted,
            Some(issuer) => current = issuer,
        }
    }

    Trust::Untrusted(format!(
        "the certificate chain is longer than {MAX_CHAIN_DEPTH} certificates"
    ))
}

/// What the cryptographic verification of one signature concluded.
enum SignatureOutcome {
    Verified(String),
    Failed(String),
    /// Neither success nor failure: no supported algorithm, or no key.
    NotChecked(String),
}

/// Copy the signer certificate's details into the report.
fn fill_certificate_details(report: &mut SignatureReport, cert: &X509Certificate<'_>) {
    if let Some(name) = common_name(cert.subject()) {
        report.signer = Some(name);
    }
    report.issuer = common_name(cert.issuer());
    report.valid_from = format_asn1_time(cert.validity().not_before.timestamp());
    report.valid_until = format_asn1_time(cert.validity().not_after.timestamp());
}

/// A name's first common name, when it has one.
fn common_name(name: &X509Name<'_>) -> Option<String> {
    name.iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_string)
}

/// An ASN.1 time as `YYYY-MM-DD HH:MM:SS UTC`.
fn format_asn1_time(timestamp: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
}

/// A PDF date `/M` value — `D:YYYYMMDDHHmmSS`, optionally followed by `Z` or
/// `+HH'mm` — as `YYYY-MM-DD HH:MM:SS ±HH:MM`.
fn format_pdf_date(raw: &str) -> Option<String> {
    let body = raw.strip_prefix('D').unwrap_or(raw);
    let body = body.strip_prefix(':').unwrap_or(body);

    let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return None;
    }

    // Fields default the way the format says: month and day to 1, the time
    // to midnight, when a producer truncated the value.
    let field = |from: usize, default: i32| -> i32 {
        digits
            .get(from..from + 2)
            .and_then(|slice| slice.parse().ok())
            .unwrap_or(default)
    };
    let year: i32 = digits[..4].parse().ok()?;
    let (month, day) = (field(4, 1), field(6, 1));
    let (hour, minute, second) = (field(8, 0), field(10, 0), field(12, 0));

    let zone = &body[digits.len()..];
    let offset = match zone.strip_prefix(['+', '-']) {
        Some(hours) => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let hour: i32 = hours.get(..2).and_then(|h| h.parse().ok()).unwrap_or(0);
            let minute: i32 = hours.get(3..5).and_then(|m| m.parse().ok()).unwrap_or(0);
            chrono::FixedOffset::east_opt(sign * (hour * 3600 + minute * 60))?
        }
        None => chrono::FixedOffset::east_opt(0)?,
    };

    let naive = chrono::NaiveDate::from_ymd_opt(year, month as u32, day as u32)?.and_hms_opt(
        hour as u32,
        minute as u32,
        second as u32,
    )?;
    // The clock time is wall time in the stated zone; show it as the signer
    // wrote it rather than re-expressing the instant elsewhere.
    use chrono::TimeZone as _;
    let time = offset.from_local_datetime(&naive).single()?;
    Some(time.format("%Y-%m-%d %H:%M:%S %:z").to_string())
}

/// The two byte spans a signature commits to.
struct ByteRange {
    first: (usize, usize),
    second: (usize, usize),
}

impl ByteRange {
    /// Four non-negative offsets that stay inside `file_len`, in order, with
    /// the second span beginning no earlier than the first ends.
    fn parse(values: Option<&[i64]>, file_len: usize) -> Option<Self> {
        let [o1, l1, o2, l2]: [i64; 4] = values?.try_into().ok()?;
        if o1 < 0 || l1 < 0 || o2 < 0 || l2 < 0 {
            return None;
        }
        let (o1, l1, o2, l2) = (o1 as usize, l1 as usize, o2 as usize, l2 as usize);
        if o1.checked_add(l1)? <= o2 && o2.checked_add(l2)? <= file_len {
            Some(Self {
                first: (o1, l1),
                second: (o2, l2),
            })
        } else {
            None
        }
    }
}

// ------------------------------------------------------------------ hashing

/// The digest algorithms signature producers actually use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigestAlg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

/// OIDs, dotted-decimal, as `AlgorithmIdentifier.algorithm` values.
const OID_SHA1: &str = "1.3.14.3.2.26";
const OID_SHA256: &str = "2.16.840.1.101.3.4.2.1";
const OID_SHA384: &str = "2.16.840.1.101.3.4.2.2";
const OID_SHA512: &str = "2.16.840.1.101.3.4.2.3";

impl DigestAlg {
    fn from_oid(oid: &str) -> Option<Self> {
        match oid {
            OID_SHA1 => Some(Self::Sha1),
            OID_SHA256 => Some(Self::Sha256),
            OID_SHA384 => Some(Self::Sha384),
            OID_SHA512 => Some(Self::Sha512),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
            Self::Sha384 => "SHA-384",
            Self::Sha512 => "SHA-512",
        }
    }

    /// The digest over both spans.
    fn digest_ranges(self, range: &ByteRange, bytes: &[u8]) -> Vec<u8> {
        let (o1, l1) = range.first;
        let (o2, l2) = range.second;
        match self {
            Self::Sha1 => digest_of::<Sha1>(&bytes[o1..o1 + l1], &bytes[o2..o2 + l2]),
            Self::Sha256 => digest_of::<Sha256>(&bytes[o1..o1 + l1], &bytes[o2..o2 + l2]),
            Self::Sha384 => digest_of::<Sha384>(&bytes[o1..o1 + l1], &bytes[o2..o2 + l2]),
            Self::Sha512 => digest_of::<Sha512>(&bytes[o1..o1 + l1], &bytes[o2..o2 + l2]),
        }
    }

    /// The digest of one blob, as needed for the authenticated attributes.
    fn digest_bytes(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha1 => Sha1::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
            Self::Sha384 => Sha384::digest(data).to_vec(),
            Self::Sha512 => Sha512::digest(data).to_vec(),
        }
    }

    /// The DER `DigestInfo` prefix for an RSASSA-PKCS1-v1_5 signature over a
    /// digest of this kind: `SEQUENCE { SEQUENCE { OID, NULL }, OCTET STRING }`,
    /// with the digest appended by the caller.
    fn digest_info_prefix(self) -> &'static [u8] {
        match self {
            Self::Sha1 => &[
                0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04,
                0x14,
            ],
            Self::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            Self::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            Self::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
        }
    }
}

/// Hash the two spans, in signing order.
fn digest_of<D: Digest>(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut hasher = D::new();
    hasher.update(first);
    hasher.update(second);
    hasher.finalize().to_vec()
}

/// Whether an encapsulated digest matches: bare, or wrapped in the DER
/// `DigestInfo` block the older PKCS#7 packaging used.
fn encapsulated_covers(encapsulated: &[u8], digest: &[u8]) -> bool {
    encapsulated == digest || (encapsulated.len() > digest.len() && encapsulated.ends_with(digest))
}

// -------------------------------------------------------------- CMS parsing

// OIDs, dotted-decimal, of the CMS structures walked below.
const OID_SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
const OID_MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";
const OID_RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";
const OID_PRIME256V1: &str = "1.2.840.10045.3.1.7";

/// The parts of a CMS `SignedData` that signature verification needs.
struct Cms<'a> {
    /// The certificates carried in the `[0]` certificate set, parsed, with
    /// their DER.
    certs: Vec<(X509Certificate<'a>, &'a [u8])>,
    /// The `SignerInfo` structures; PDF signatures carry exactly one.
    signers: Vec<SignerInfo<'a>>,
    /// The encapsulated digest in `EncapsulatedContentInfo`, present for the
    /// `adbe.pkcs7.sha1` style packaging without authenticated attributes.
    econtent: Option<&'a [u8]>,
}

/// One `SignerInfo`.
struct SignerInfo<'a> {
    /// How the signer identified their certificate.
    sid: Sid<'a>,
    /// The digest algorithm, dotted OID.
    digest_alg: String,
    /// The `[0] IMPLICIT SET OF Attribute` authenticated attributes.
    signed_attrs: Option<SignedAttrs<'a>>,
    /// The signature algorithm, dotted OID.
    signature_alg: String,
    /// The signature octets.
    signature: &'a [u8],
}

/// A `SignerIdentifier`.
enum Sid<'a> {
    /// `issuerAndSerialNumber`, the usual form.
    IssuerAndSerial {
        issuer_der: &'a [u8],
        serial: &'a [u8],
    },
    /// Anything else (`[0] subjectKeyIdentifier`); the certificate is then
    /// picked by position instead.
    Other,
}

/// The authenticated attributes verification reads.
struct SignedAttrs<'a> {
    /// The full DER of the `[0]`-tagged SET OF Attribute.
    raw: &'a [u8],
    /// The `messageDigest` attribute value.
    message_digest: Option<&'a [u8]>,
}

impl SignedAttrs<'_> {
    /// The DER a signature commits to: the raw attributes with the `[0]` tag
    /// byte replaced by universal SET, as CMS specifies.
    fn signed_der(&self) -> Vec<u8> {
        let mut der = self.raw.to_vec();
        if der.first() == Some(&0xA0) {
            der[0] = 0x31;
        }
        der
    }
}

impl<'a> Cms<'a> {
    /// Parse a `ContentInfo` wrapping a `SignedData`, ignoring any zero
    /// padding producers leave after the object.
    fn parse(contents: &'a [u8]) -> Parse<Self> {
        let (content_info, _, _) = take(contents)?;
        if !is_universal(&content_info, Tag::Sequence) {
            return Err("the object is not a ContentInfo SEQUENCE".into());
        }

        let mut info = children(content_info.as_bytes()).into_iter();
        let content_type = info
            .next()
            .and_then(|(any, _)| oid_string(&any))
            .ok_or("the ContentInfo carries no contentType")?;
        if content_type != OID_SIGNED_DATA {
            return Err(format!("contentType {content_type} is not id-signedData"));
        }

        let (wrapped, _) = info.next().ok_or("the ContentInfo carries no content")?;
        if !is_context(&wrapped, 0) {
            return Err("the content is not [0]-tagged".into());
        }
        let (signed_data, _, _) = take(wrapped.as_bytes())?;
        if !is_universal(&signed_data, Tag::Sequence) {
            return Err("the content is not a SignedData SEQUENCE".into());
        }

        let mut data = children(signed_data.as_bytes()).into_iter();
        let version = data.next().ok_or("the SignedData carries no version")?;
        if version.0.tag() != Tag::Integer {
            return Err("the SignedData version is not an INTEGER".into());
        }

        let digest_algorithms = data
            .next()
            .ok_or("the SignedData carries no digestAlgorithms set")?;
        if !is_universal(&digest_algorithms.0, Tag::Set) {
            return Err("digestAlgorithms is not a SET".into());
        }

        let encap = data
            .next()
            .ok_or("the SignedData carries no encapContentInfo")?;
        if !is_universal(&encap.0, Tag::Sequence) {
            return Err("encapContentInfo is not a SEQUENCE".into());
        }
        let econtent = parse_econtent(&encap.0)?;

        // Remaining children in order: [0] certificates, [1] CRLs, then the
        // signerInfos SET. Walk by tag so absent optionals are skipped.
        let mut certs = Vec::new();
        let mut signers = Vec::new();
        for (any, _) in data {
            if is_context(&any, 0) {
                certs = parse_certificates(any.as_bytes())?;
            } else if is_universal(&any, Tag::Set) {
                for (signer, _) in children(any.as_bytes()) {
                    if !is_universal(&signer, Tag::Sequence) {
                        return Err("a SignerInfo is not a SEQUENCE".into());
                    }
                    signers.push(SignerInfo::parse(&signer)?);
                }
            }
            // [1] CRLs and anything unrecognised is skipped.
        }

        if signers.is_empty() {
            return Err("the SignedData carries no SignerInfo".into());
        }

        Ok(Self {
            certs,
            signers,
            econtent,
        })
    }

    /// The certificate the signer pointed at: matched by issuer and serial
    /// when the identifier names them, else the first one carried.
    fn signer_certificate(
        &self,
        signer: &SignerInfo<'_>,
    ) -> Option<&(X509Certificate<'_>, &'_ [u8])> {
        let chosen = match &signer.sid {
            Sid::IssuerAndSerial { issuer_der, serial } => self
                .certs
                .iter()
                .find(|(cert, _)| {
                    serial_matches(cert.raw_serial(), serial)
                        && X509Name::from_der(issuer_der)
                            .map(|(_, name)| name == *cert.issuer())
                            .unwrap_or(false)
                })
                .or_else(|| self.certs.first()),
            Sid::Other => self.certs.first(),
        };
        chosen
    }
}

impl<'a> SignerInfo<'a> {
    fn parse(any: &Any<'a>) -> Parse<Self> {
        let mut kids = children(any.as_bytes()).into_iter();

        let version = kids.next().ok_or("the SignerInfo carries no version")?;
        if version.0.tag() != Tag::Integer {
            return Err("the SignerInfo version is not an INTEGER".into());
        }

        let sid = kids
            .next()
            .ok_or("the SignerInfo carries no signer identifier")?;
        let sid = if is_universal(&sid.0, Tag::Sequence) {
            let mut parts = children(sid.0.as_bytes()).into_iter();
            let issuer = parts
                .next()
                .ok_or("issuerAndSerialNumber carries no issuer")?;
            let serial = parts
                .next()
                .ok_or("issuerAndSerialNumber carries no serial")?;
            if serial.0.tag() != Tag::Integer {
                return Err("the issuer serial is not an INTEGER".into());
            }
            Sid::IssuerAndSerial {
                issuer_der: issuer.1,
                serial: serial.0.as_bytes(),
            }
        } else {
            Sid::Other
        };

        let digest_alg = kids
            .next()
            .and_then(|(any, _)| algorithm_oid(&any))
            .ok_or("the SignerInfo carries no digest algorithm")?;

        // What remains: optional [0] authenticated attributes, then the
        // signature algorithm SEQUENCE and signature OCTET STRING. The [1]
        // unsigned attributes, if present, trail after and are skipped.
        let mut signed_attrs = None;
        let mut signature_alg = None;
        let mut signature = None;
        for (any, raw) in kids {
            if is_context(&any, 0) && signature_alg.is_none() {
                signed_attrs = Some(parse_signed_attrs(raw)?);
            } else if is_universal(&any, Tag::Sequence) && signature_alg.is_none() {
                signature_alg = algorithm_oid(&any);
            } else if is_universal(&any, Tag::OctetString) {
                signature = Some(any.as_bytes());
            }
        }

        Ok(Self {
            sid,
            digest_alg,
            signed_attrs,
            signature_alg: signature_alg.ok_or("the SignerInfo carries no signature algorithm")?,
            signature: signature.ok_or("the SignerInfo carries no signature")?,
        })
    }
}

/// The encapsulated digest, when `EncapsulatedContentInfo` carries one.
fn parse_econtent<'a>(encap: &Any<'a>) -> Parse<Option<&'a [u8]>> {
    for (any, _) in children(encap.as_bytes()).into_iter().skip(1) {
        if is_context(&any, 0) {
            let (octets, _, _) = take(any.as_bytes())?;
            if !is_universal(&octets, Tag::OctetString) {
                return Err("eContent is not an OCTET STRING".into());
            }
            return Ok(Some(octets.as_bytes()));
        }
    }
    Ok(None)
}

/// The `[0] IMPLICIT SET OF Certificate` content: concatenated certificate
/// DER objects. Zero padding after the last certificate is normal — producers
/// reserve the whole `/Contents` hole — so an all-zero remainder ends the set.
fn parse_certificates(content: &[u8]) -> Parse<Vec<(X509Certificate<'_>, &'_ [u8])>> {
    let mut certs = Vec::new();
    let mut input = content;
    while !input.iter().all(|byte| *byte == 0) {
        match X509Certificate::from_der(input) {
            Ok((rest, cert)) => {
                let raw = &input[..input.len() - rest.len()];
                certs.push((cert, raw));
                input = rest;
            }
            Err(_) => return Err("a certificate in the CMS is not parseable DER".into()),
        }
    }
    Ok(certs)
}

/// One `[0] IMPLICIT SET OF Attribute`: the messageDigest value, plus the
/// raw DER (whose tag the signature replaces with SET).
fn parse_signed_attrs(raw: &[u8]) -> Parse<SignedAttrs<'_>> {
    if raw.first() != Some(&0xA0) {
        return Err("the signed attributes are not [0]-tagged".into());
    }

    let mut message_digest = None;
    for (attribute, _) in children(&raw[1 + length_of_length(&raw[1..])..]) {
        let mut parts = children(attribute.as_bytes()).into_iter();
        let Some(oid) = parts.next().and_then(|(any, _)| oid_string(&any)) else {
            continue;
        };
        let Some(values) = parts.next() else {
            continue;
        };
        let Some((value, _)) = children(values.0.as_bytes()).into_iter().next() else {
            continue;
        };

        if oid == OID_MESSAGE_DIGEST && value.tag() == Tag::OctetString {
            message_digest = Some(value.as_bytes());
        }
    }

    Ok(SignedAttrs {
        raw,
        message_digest,
    })
}

// ------------------------------------------------------- crypto verification

/// Verify the signer's signature, which covers the authenticated attributes
/// when present, else the signed content itself; `digest` is the digest of
/// exactly that.
fn verify_signature(
    cert: &X509Certificate<'_>,
    signer: &SignerInfo<'_>,
    digest: &[u8],
    digest_alg: DigestAlg,
) -> SignatureOutcome {
    let key = cert.public_key().parsed();
    let key = match key {
        Ok(key) => key,
        Err(reason) => return SignatureOutcome::NotChecked(reason.to_string()),
    };

    match key {
        PublicKey::RSA(key) => verify_rsa(&key, signer, digest, digest_alg),
        PublicKey::EC(point) => verify_p256(cert, point.data(), signer, digest),
        _ => SignatureOutcome::NotChecked("unsupported public key algorithm".into()),
    }
}

/// RSASSA-PKCS1-v1_5 over the digest. The `rsa` crate is taken unprefixed
/// (`Pkcs1v15Sign::new_unprefixed`) with the standard `DigestInfo` block
/// applied by hand, which is exactly what a conforming signer produces.
fn verify_rsa(
    key: &x509_parser::public_key::RSAPublicKey<'_>,
    signer: &SignerInfo<'_>,
    digest: &[u8],
    digest_alg: DigestAlg,
) -> SignatureOutcome {
    let scheme = match signer.signature_alg.as_str() {
        // rsaEncryption and the sha*WithRSAEncryption family are all
        // RSASSA-PKCS1-v1_5; the digest comes from the SignerInfo.
        OID_RSA_ENCRYPTION
        | "1.2.840.113549.1.1.5"
        | "1.2.840.113549.1.1.11"
        | "1.2.840.113549.1.1.12"
        | "1.2.840.113549.1.1.13"
        | "1.2.840.113549.1.1.14" => rsa::pkcs1v15::Pkcs1v15Sign::new_unprefixed(),
        other => {
            return SignatureOutcome::NotChecked(format!(
                "signature algorithm {other} is not supported"
            ))
        }
    };

    let modulus = rsa::BigUint::from_bytes_be(trim_leading_zero(key.modulus));
    let exponent = rsa::BigUint::from_bytes_be(trim_leading_zero(key.exponent));
    let bits = modulus.bits();
    let public = match rsa::RsaPublicKey::new(modulus, exponent) {
        Ok(public) => public,
        Err(reason) => return SignatureOutcome::NotChecked(format!("unusable RSA key: {reason}")),
    };

    let mut digest_info = digest_alg.digest_info_prefix().to_vec();
    digest_info.extend_from_slice(digest);

    let detail = format!("RSA-{bits} PKCS#1 v1.5, {}", digest_alg.name());
    match public.verify(scheme, &digest_info, signer.signature) {
        Ok(()) => SignatureOutcome::Verified(detail),
        Err(_) => SignatureOutcome::Failed(detail),
    }
}

/// ECDSA over P-256; the signature DER carries `r` and `s`. Other curves fall
/// back to the digest-only verdict.
fn verify_p256(
    cert: &X509Certificate<'_>,
    sec1_point: &[u8],
    signer: &SignerInfo<'_>,
    digest: &[u8],
) -> SignatureOutcome {
    // The curve is named in the key algorithm's parameters; only P-256 is
    // implemented.
    let curve = cert
        .public_key()
        .algorithm
        .parameters
        .as_ref()
        .and_then(|any| any.as_oid().ok())
        .map(|oid| oid.to_string());
    if curve.as_deref() != Some(OID_PRIME256V1) {
        return SignatureOutcome::NotChecked(format!(
            "EC public key on an unsupported curve ({})",
            curve.as_deref().unwrap_or("unnamed")
        ));
    }

    // `verify_prehash` takes the digest of the signed content directly,
    // truncating or zero-padding to the field size as the curve requires.
    let verifies = (|| {
        use p256::ecdsa::signature::hazmat::PrehashVerifier;

        let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(sec1_point).ok()?;
        let signature = p256::ecdsa::Signature::from_der(signer.signature).ok()?;
        key.verify_prehash(digest, &signature).ok()
    })();

    match verifies {
        Some(()) => SignatureOutcome::Verified("ECDSA P-256".into()),
        None => SignatureOutcome::Failed("ECDSA P-256".into()),
    }
}

// --------------------------------------------------------------- DER helpers

/// Split one TLV off `input`: the parsed `Any`, its raw DER, and the rest.
fn take(input: &[u8]) -> Parse<(Any<'_>, &'_ [u8], &'_ [u8])> {
    let (rest, any) = Any::from_der(input).map_err(|reason| reason.to_string())?;
    let len = input.len() - rest.len();
    Ok((any, &input[..len], rest))
}

/// The TLVs packed inside a constructed element's content. Parsing stops at
/// the first malformed child or at the end of the content.
fn children(content: &[u8]) -> Vec<(Any<'_>, &[u8])> {
    let mut out = Vec::new();
    let mut input = content;
    while !input.is_empty() {
        match Any::from_der(input) {
            Ok((rest, any)) => {
                let raw = &input[..input.len() - rest.len()];
                out.push((any, raw));
                input = rest;
            }
            Err(_) => break,
        }
    }
    out
}

fn is_universal(any: &Any<'_>, tag: Tag) -> bool {
    any.class() == Class::Universal && any.tag() == tag
}

fn is_context(any: &Any<'_>, number: u32) -> bool {
    any.class() == Class::ContextSpecific && any.tag() == Tag(number)
}

/// A dotted-decimal OID, when the element is one.
fn oid_string(any: &Any<'_>) -> Option<String> {
    any.as_oid().ok().map(|oid| oid.to_string())
}

/// The OID inside an `AlgorithmIdentifier`, when the element is one.
fn algorithm_oid(any: &Any<'_>) -> Option<String> {
    children(any.as_bytes())
        .into_iter()
        .next()
        .and_then(|(child, _)| oid_string(&child))
}

/// Serial numbers compared as unsigned integers: DER INTEGERs carry a leading
/// zero when the high bit would make them look negative.
fn serial_matches(cert_serial: &[u8], sid_serial: &[u8]) -> bool {
    trim_leading_zero(cert_serial) == trim_leading_zero(sid_serial)
}

fn trim_leading_zero(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len());
    &bytes[end..]
}

/// The number of bytes `der` spends encoding a DER length: `der` starts at
/// the first length byte.
fn length_of_length(der: &[u8]) -> usize {
    match der.first() {
        Some(byte) if byte & 0x80 != 0 => 1 + (byte & 0x7f) as usize,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_are_bounds_checked() {
        assert!(ByteRange::parse(Some(&[0, 10, 20, 10]), 30).is_some());
        assert!(ByteRange::parse(Some(&[0, 10, 10, 10]), 30).is_some());
        // Overlapping spans, past the end, negative, wrong arity: all rejected.
        assert!(ByteRange::parse(Some(&[0, 15, 10, 10]), 30).is_none());
        assert!(ByteRange::parse(Some(&[0, 10, 20, 11]), 30).is_none());
        assert!(ByteRange::parse(Some(&[0, -1, 20, 10]), 30).is_none());
        assert!(ByteRange::parse(Some(&[0, 10]), 30).is_none());
        assert!(ByteRange::parse(None, 30).is_none());
    }

    #[test]
    fn pdf_dates_parse_with_and_without_zones() {
        assert_eq!(
            format_pdf_date("D:20260102130405Z"),
            Some("2026-01-02 13:04:05 +00:00".to_string())
        );
        assert_eq!(
            format_pdf_date("D:20260102130405+02'00'"),
            Some("2026-01-02 13:04:05 +02:00".to_string())
        );
        assert_eq!(
            format_pdf_date("D:20260102130405-05'30'"),
            Some("2026-01-02 13:04:05 -05:30".to_string())
        );
        // Year only, the minimum the format allows, and midnight defaults.
        assert_eq!(
            format_pdf_date("D:2026"),
            Some("2026-01-01 00:00:00 +00:00".to_string())
        );
        assert_eq!(
            format_pdf_date("D:20260102"),
            Some("2026-01-02 00:00:00 +00:00".to_string())
        );
        assert_eq!(format_pdf_date("not a date"), None);
    }

    #[test]
    fn digest_info_prefixes_match_the_standard_blocks() {
        assert_eq!(
            DigestAlg::Sha256.digest_info_prefix(),
            &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ]
        );
        assert_eq!(
            DigestAlg::Sha1.digest_info_prefix()[11..],
            [0x05, 0x00, 0x04, 0x14]
        );
    }

    #[test]
    fn leading_zeroes_do_not_change_serial_equality() {
        assert!(serial_matches(&[0x01, 0x00], &[0x00, 0x01, 0x00]));
        assert!(!serial_matches(&[0x01, 0x00], &[0x01, 0x01]));
        assert!(serial_matches(&[0x00, 0x00], &[0x00]));
    }

    #[test]
    fn der_length_parsing_skips_long_forms() {
        assert_eq!(length_of_length(&[0x10, 0x01, 0x02]), 1);
        assert_eq!(length_of_length(&[0x82, 0x01, 0x00, 0x00]), 3);
    }

    #[test]
    fn known_digests_reproduce_test_vectors() {
        // SHA-256 of the empty string and of "abc" (FIPS 180 vectors), and
        // the same two for SHA-1, tying the streaming byte-range hasher to
        // published values.
        let empty = ByteRange {
            first: (0, 0),
            second: (0, 0),
        };
        let nothing = b"";
        assert_eq!(
            DigestAlg::digest_ranges(DigestAlg::Sha256, &empty, nothing),
            hex("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(
            DigestAlg::digest_ranges(DigestAlg::Sha1, &empty, nothing),
            hex("da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );

        let abc = ByteRange {
            first: (0, 3),
            second: (0, 0),
        };
        assert_eq!(
            DigestAlg::digest_ranges(DigestAlg::Sha256, &abc, b"abc\0"),
            hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(
            DigestAlg::digest_ranges(DigestAlg::Sha1, &abc, b"abc\0"),
            hex("a9993e364706816aba3e25717850c26c9cd0d89d")
        );
    }

    #[test]
    fn encapsulated_digests_match_bare_and_digestinfo_wrapped() {
        let digest = [0xABu8; 32];
        assert!(encapsulated_covers(&digest, &digest));
        let mut digest_info = DigestAlg::Sha256.digest_info_prefix().to_vec();
        digest_info.extend_from_slice(&digest);
        assert!(encapsulated_covers(&digest_info, &digest));
        let wrong = [0xCDu8; 32];
        assert!(!encapsulated_covers(&wrong, &digest));
    }

    fn hex(bytes: &str) -> Vec<u8> {
        (0..bytes.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&bytes[at..at + 2], 16).expect("valid hex"))
            .collect()
    }
}
