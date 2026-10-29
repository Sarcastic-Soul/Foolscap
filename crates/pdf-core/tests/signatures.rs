//! Digital signature verification. Only built with the `signatures` feature.
//!
//! No real signed PDFs are committed, so the fixtures are generated — up to
//! and including the cryptography: the test mints an RSA key, wraps it in a
//! hand-built self-signed X.509 certificate, packs both into a hand-built CMS
//! `SignedData`, and splices that into the zero-padded `/Contents` hole of a
//! saved PDF over a real `/ByteRange`. The ladder:
//!
//! 1. A plain document reports no signatures.
//! 2. A signature-shaped `/V` with opaque contents is reported honestly as
//!    unparseable, and a missing `/ByteRange` likewise.
//! 3. The crypto rung: a detached `adbe.pkcs7.detached` signature with
//!    authenticated attributes verifies as intact (digest and RSA signature
//!    both checked), the same file with one signed byte flipped is broken,
//!    and a digest that matches with a signature made over other bytes fails
//!    on the signature alone. A legacy `adbe.pkcs7.sha1` signature whose
//!    digest is encapsulated in `eContent` verifies too.

#![cfg(feature = "signatures")]

mod common;

use std::sync::OnceLock;

use common::{Attributes, Workspace, A4};
use lopdf::{dictionary, Document, Object, StringFormat};
use pdf_core::verify_signatures;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::rand_core::{CryptoRng, RngCore};
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha256};

// -------------------------------------------------------------- DER writing

/// TLV-encoded DER: a tag and its length-prefixed content.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let length = if content.len() < 0x80 {
        vec![content.len() as u8]
    } else {
        let bytes = content.len().to_be_bytes();
        let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
        let mut encoded = vec![0x80 | (8 - first) as u8];
        encoded.extend_from_slice(&bytes[first..]);
        encoded
    };

    let mut out = Vec::with_capacity(2 + length.len() + content.len());
    out.push(tag);
    out.extend_from_slice(&length);
    out.extend_from_slice(content);
    out
}

/// An OBJECT IDENTIFIER from its arcs.
fn oid(arcs: &[u64]) -> Vec<u8> {
    let mut body = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for arc in &arcs[2..] {
        // Groups of seven bits, least significant first; DER wants most
        // significant first with the continuation bit on all but the last.
        let mut groups = vec![(arc & 0x7f) as u8];
        let mut value = arc >> 7;
        while value != 0 {
            groups.push((value & 0x7f) as u8);
            value >>= 7;
        }
        groups.reverse();
        for (index, group) in groups.iter().enumerate() {
            let last = index + 1 == groups.len();
            body.push(if last { *group } else { group | 0x80 });
        }
    }
    tlv(0x06, &body)
}

/// An unsigned INTEGER, DER's leading-zero rule applied.
fn integer(bytes: &[u8]) -> Vec<u8> {
    let stripped = bytes
        .iter()
        .position(|byte| *byte != 0)
        .map(|at| &bytes[at..])
        .unwrap_or(&bytes[bytes.len() - 1..]);
    let mut content = stripped.to_vec();
    if stripped[0] & 0x80 != 0 {
        content.insert(0, 0);
    }
    tlv(0x02, &content)
}

/// An AlgorithmIdentifier with a NULL parameter.
fn algid(arcs: &[u64]) -> Vec<u8> {
    tlv(0x30, &[oid(arcs), tlv(0x05, &[])].concat())
}

/// A Name holding one common name RDN.
fn name_cn(cn: &str) -> Vec<u8> {
    let atv = tlv(
        0x30,
        &[oid(&[2, 5, 4, 3]), tlv(0x0C, cn.as_bytes())].concat(),
    );
    tlv(0x30, &tlv(0x31, &atv))
}

/// A BIT STRING with no unused bits.
fn bit_string(data: &[u8]) -> Vec<u8> {
    tlv(0x03, &[&[0u8], data].concat())
}

// --------------------------------------------------------------- test signer

/// A deterministic RNG, so the fixture key is reproducible without pulling a
/// random-number crate in behind the feature.
struct FixtureRng(u64);

impl RngCore for FixtureRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for FixtureRng {}

/// The one key every signing fixture shares, in its self-signed certificate.
struct Signer {
    key: RsaPrivateKey,
    /// The certificate DER, as carried in the CMS.
    cert_der: Vec<u8>,
    /// The issuer Name DER and serial, for the SignerIdentifier.
    issuer_der: Vec<u8>,
    serial_der: Vec<u8>,
}

impl Signer {
    /// RSASSA-PKCS1-v1_5 over a digest: the standard DigestInfo block plus
    /// the digest, signed unprefixed — what any conforming signer produces.
    fn sign(&self, digest_info_prefix: &[u8], digest: &[u8]) -> Vec<u8> {
        let mut digest_info = digest_info_prefix.to_vec();
        digest_info.extend_from_slice(digest);
        self.key
            .sign(Pkcs1v15Sign::new_unprefixed(), &digest_info)
            .expect("the fixture key signs")
    }
}

/// DigestInfo prefixes, duplicated from the module under test on purpose: the
/// test would prove nothing if it borrowed the constants it checks.
const SHA1_PREFIX: &[u8] = &[
    0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04, 0x14,
];
const SHA256_PREFIX: &[u8] = &[
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

const OID_SHA1: &[u64] = &[1, 3, 14, 3, 2, 26];
const OID_SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];
const OID_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
const OID_SHA1_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 5];
const OID_SHA256_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 11];
const OID_SIGNED_DATA: &[u64] = &[1, 2, 840, 113549, 1, 7, 2];
const OID_DATA: &[u64] = &[1, 2, 840, 113549, 1, 7, 1];
const OID_ATTR_CONTENT_TYPE: &[u64] = &[1, 2, 840, 113549, 1, 9, 3];
const OID_ATTR_MESSAGE_DIGEST: &[u64] = &[1, 2, 840, 113549, 1, 9, 4];

/// How the fixture's CMS is signed.
enum Plan {
    /// `adbe.pkcs7.detached`: authenticated attributes carry the message
    /// digest, the RSA signature covers their DER.
    Detached,
    /// `adbe.pkcs7.sha1`: no attributes, the SHA-1 digest rides in eContent
    /// and the signature covers the digest.
    Encapsulated,
    /// Detached in shape, but signed over other bytes than the attributes —
    /// a digest that matches and a signature that cannot.
    Forged,
}

/// The shared signer, with its certificate built by hand: a v3 TBS with the
/// RSA key, valid 2026 through 2030, signed with itself.
fn signer() -> &'static Signer {
    static SIGNER: OnceLock<Signer> = OnceLock::new();
    SIGNER.get_or_init(|| {
        let mut rng = FixtureRng(0x243F_6A88_85A3_08D3);
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("could not generate the fixture key");

        let name = name_cn("Foolscap Fixture Signer");
        let rsa_public = tlv(
            0x30,
            &[
                integer(&key.n().to_bytes_be()),
                integer(&key.e().to_bytes_be()),
            ]
            .concat(),
        );
        let spki = tlv(0x30, &[algid(OID_RSA), bit_string(&rsa_public)].concat());
        let validity = tlv(
            0x30,
            &[tlv(0x17, b"260101000000Z"), tlv(0x17, b"301231235959Z")].concat(),
        );

        let serial = integer(&[0x2A]);
        let mut tbs = Vec::new();
        tbs.extend(tlv(0xA0, &integer(&[2]))); // [0] EXPLICIT version: v3
        tbs.extend(serial.clone());
        tbs.extend(algid(OID_SHA256_RSA));
        tbs.extend(name.clone()); // issuer
        tbs.extend(validity);
        tbs.extend(name.clone()); // subject: self-signed
        tbs.extend(spki);
        let tbs = tlv(0x30, &tbs);

        let mut tbs_digest_info = SHA256_PREFIX.to_vec();
        tbs_digest_info.extend_from_slice(&Sha256::digest(&tbs));
        let signature = key
            .sign(Pkcs1v15Sign::new_unprefixed(), &tbs_digest_info)
            .expect("the fixture certificate signs");
        let cert_der = tlv(
            0x30,
            &[tbs, algid(OID_SHA256_RSA), bit_string(&signature)].concat(),
        );

        Signer {
            key,
            cert_der,
            issuer_der: name,
            serial_der: serial,
        }
    })
}

/// Build the CMS `SignedData` for a fixture under `plan`.
fn build_cms(signer: &Signer, digest: &[u8], plan: &Plan) -> Vec<u8> {
    let (digest_alg, signature_alg, prefix) = match plan {
        Plan::Encapsulated => (OID_SHA1, OID_SHA1_RSA, SHA1_PREFIX),
        _ => (OID_SHA256, OID_SHA256_RSA, SHA256_PREFIX),
    };

    // The authenticated attributes, when the plan uses them.
    let signed_attrs = match plan {
        Plan::Encapsulated => None,
        _ => {
            let content_type = tlv(
                0x30,
                &[oid(OID_ATTR_CONTENT_TYPE), tlv(0x31, &oid(OID_DATA))].concat(),
            );
            let message_digest = tlv(
                0x30,
                &[oid(OID_ATTR_MESSAGE_DIGEST), tlv(0x31, &tlv(0x04, digest))].concat(),
            );
            Some(tlv(0xA0, &[content_type, message_digest].concat()))
        }
    };

    // What the RSA signature covers.
    let signature = match plan {
        // The DER of the attributes, with the [0] tag changed to SET.
        Plan::Detached => {
            let mut der = signed_attrs
                .clone()
                .expect("detached plans sign attributes");
            der[0] = 0x31;
            signer.sign(prefix, &Sha256::digest(&der))
        }
        Plan::Encapsulated => signer.sign(prefix, digest),
        Plan::Forged => signer.sign(prefix, &Sha256::digest(b"a forgery")),
    };

    let mut signer_info = Vec::new();
    signer_info.extend(integer(&[1])); // version
    signer_info.extend(tlv(
        0x30,
        &[signer.issuer_der.clone(), signer.serial_der.clone()].concat(),
    ));
    signer_info.extend(algid(digest_alg));
    if let Some(attrs) = &signed_attrs {
        signer_info.extend_from_slice(attrs);
    }
    signer_info.extend(algid(signature_alg));
    signer_info.extend(tlv(0x04, &signature));

    let encap_content = match plan {
        // Detached: no eContent; the digest travels in the attributes.
        Plan::Detached | Plan::Forged => tlv(0x30, &oid(OID_DATA)),
        // The legacy sha1 packaging encapsulates the digest.
        Plan::Encapsulated => tlv(
            0x30,
            &[oid(OID_DATA), tlv(0xA0, &tlv(0x04, digest))].concat(),
        ),
    };

    let mut signed_data = Vec::new();
    signed_data.extend(integer(&[1])); // version
    signed_data.extend(tlv(0x31, &algid(digest_alg))); // digestAlgorithms
    signed_data.extend(encap_content); // encapContentInfo
    signed_data.extend(tlv(0xA0, &signer.cert_der)); // certificates
    signed_data.extend(tlv(0x31, &tlv(0x30, &signer_info))); // signerInfos

    // ContentInfo: id-signedData wrapping the SignedData, [0] EXPLICIT.
    tlv(
        0x30,
        &[oid(OID_SIGNED_DATA), tlv(0xA0, &tlv(0x30, &signed_data))].concat(),
    )
}

// ------------------------------------------------------------------ fixtures

/// The `/Contents` hole: 2048 bytes as 4096 hex digits, the size real
/// producers reserve for an RSA-2048 CMS whose certificate and signature
/// alone account for half a kilobyte each.
const HOLE: usize = 2048;

/// The dummy `/ByteRange` as lopdf serialises it — no separator between a
/// name and an array — replaced after saving by values of the same or shorter
/// width, so the splice never moves a byte.
const DUMMY_RANGE: &str = "/ByteRange[0 99999999 99999999 99999999]";

/// A one-page document carrying a signature field whose `/V` still holds the
/// zero placeholder: listed in `/AcroForm /Fields` and in the page's
/// `/Annots`, so the two walks land on the same object.
fn placeholder_pdf(sub_filter: &str) -> Document {
    let mut doc = common::build(1, "Signed", A4, Attributes::OnPage);

    let signature_id = doc.add_object(dictionary! {
        "Type" => "Sig",
        "Filter" => "Adobe.PPKLite",
        "SubFilter" => sub_filter,
        "ByteRange" => vec![
            Object::Integer(0),
            Object::Integer(99999999),
            Object::Integer(99999999),
            Object::Integer(99999999),
        ],
        "Contents" => Object::String(vec![0u8; HOLE], StringFormat::Hexadecimal),
        "Name" => Object::string_literal("Foolscap Fixture Signer"),
        "M" => Object::string_literal("D:20261024000000Z"),
        "Reason" => Object::string_literal("fixture approval"),
    });
    let field_id = doc.add_object(dictionary! {
        "FT" => "Sig",
        "T" => Object::string_literal("Signature1"),
        "Subtype" => "Widget",
        "Rect" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(0), Object::Integer(0)],
        "V" => signature_id,
    });

    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("the fixture catalog exists");
    doc.get_dictionary_mut(root)
        .expect("the catalog is a dictionary")
        .set(
            "AcroForm",
            dictionary! { "Fields" => vec![Object::Reference(field_id)] },
        );

    let page_id = doc.get_pages()[&1];
    doc.get_dictionary_mut(page_id)
        .expect("the page is a dictionary")
        .set("Annots", vec![Object::Reference(field_id)]);

    doc
}

/// Where the reserved hole landed in the saved bytes.
struct Hole {
    /// The index of the first hex digit.
    open: usize,
    /// The index of the closing `>`.
    close: usize,
}

impl Hole {
    /// The two spans around the hole, as a real producer's `/ByteRange`.
    fn byte_range(&self, file_len: usize) -> [usize; 4] {
        [0, self.open, self.close, file_len - self.close]
    }
}

/// Save the placeholder and patch in the real `/ByteRange`, leaving every
/// byte offset where it was.
fn save_with_byte_range(doc: &mut Document) -> (Vec<u8>, Hole) {
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("could not save the fixture");

    let mut marker = Vec::with_capacity(HOLE * 2 + 2);
    marker.push(b'<');
    marker.extend(std::iter::repeat_n(b'0', HOLE * 2));
    marker.push(b'>');
    let at = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("the placeholder hole is in the saved bytes");
    let hole = Hole {
        open: at + 1,
        close: at + 1 + HOLE * 2,
    };

    let dummy = DUMMY_RANGE.as_bytes();
    let at = bytes
        .windows(dummy.len())
        .position(|window| window == dummy)
        .expect("the dummy byte range is in the saved bytes");
    let real = format!(
        "/ByteRange[0 {} {} {}]",
        hole.open,
        hole.close,
        bytes.len() - hole.close
    );
    assert!(
        real.len() <= DUMMY_RANGE.len(),
        "the real byte range must fit the dummy's width"
    );
    let padded = format!("{real:<width$}", width = DUMMY_RANGE.len());
    bytes[at..at + dummy.len()].copy_from_slice(padded.as_bytes());

    (bytes, hole)
}

/// The digest input a fixture's signer commits to: both spans of the file.
fn signed_spans(bytes: &[u8], hole: &Hole) -> Vec<u8> {
    let [_, l1, o2, l2] = hole.byte_range(bytes.len());
    [bytes[..l1].to_vec(), bytes[o2..o2 + l2].to_vec()].concat()
}

/// Splice the CMS into the hole: hex digits, zero-padded to the full width.
fn splice_cms(bytes: &mut [u8], hole: &Hole, cms: &[u8]) {
    assert!(
        cms.len() <= HOLE,
        "the fixture CMS must fit the reserved hole"
    );
    let mut hex = cms
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    hex.extend(std::iter::repeat_n('0', (HOLE - cms.len()) * 2));
    bytes[hole.open..hole.close].copy_from_slice(hex.as_bytes());
}

/// A fixture under `plan`, written into the workspace.
fn signed_pdf(
    workspace: &Workspace,
    name: &str,
    sub_filter: &str,
    plan: Plan,
) -> std::path::PathBuf {
    let signer = signer();
    let (mut bytes, hole) = save_with_byte_range(&mut placeholder_pdf(sub_filter));

    let spans = signed_spans(&bytes, &hole);
    let digest = match plan {
        Plan::Encapsulated => sha1::Sha1::digest(&spans).to_vec(),
        _ => Sha256::digest(&spans).to_vec(),
    };
    let cms = build_cms(signer, &digest, &plan);
    splice_cms(&mut bytes, &hole, &cms);

    let path = workspace.join(name);
    std::fs::write(&path, bytes).expect("could not write the fixture");
    path
}

/// Flip one byte inside the signed spans — `/Reason`, which sits after the
/// hole in the same object, keeping the file parseable.
fn tamper(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).expect("could not read the fixture");
    let at = bytes
        .windows(b"fixture approval".len())
        .position(|window| window == b"fixture approval")
        .expect("the reason string is in the fixture");
    bytes[at] = b't';
    std::fs::write(path, bytes).expect("could not rewrite the fixture");
}

// -------------------------------------------------------------------- tests

#[test]
fn unsigned_documents_report_no_signatures() {
    let workspace = Workspace::new();
    let plain = workspace.document("plain.pdf", 2, "Page");
    let doc = pdf_core::Document::open(&plain).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert!(
        reports.is_empty(),
        "a plain document has no signatures: {reports:?}"
    );
}

#[test]
fn text_fields_are_not_signatures() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Form", A4, Attributes::OnPage);
    common::add_annotation(
        &mut doc,
        1,
        "Widget",
        [0, 0, 100, 20],
        common::Normal::Text("hello"),
        0,
    );
    let path = workspace.write("form.pdf", doc);
    let doc = pdf_core::Document::open(&path).unwrap();
    assert!(verify_signatures(&doc).unwrap().is_empty());
}

#[test]
fn opaque_contents_are_reported_as_unparseable() {
    let workspace = Workspace::new();
    // A signature-shaped /V whose hole was never filled: the byte range is
    // real, the contents are not DER.
    let (bytes, _) = save_with_byte_range(&mut placeholder_pdf("adbe.pkcs7.detached"));
    let path = workspace.join("opaque.pdf");
    std::fs::write(&path, bytes).expect("could not write the fixture");

    let doc = pdf_core::Document::open(&path).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].intact);
    assert!(
        reports[0].reason.contains("could not parse the CMS"),
        "{}",
        reports[0].reason
    );
    assert!(reports[0].reason.contains("chain-of-trust was NOT checked"));
}

#[test]
fn a_missing_byte_range_is_reported() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Sparse", A4, Attributes::OnPage);
    let signature_id = doc.add_object(dictionary! {
        "Type" => "Sig",
        "Filter" => "Adobe.PPKLite",
        "SubFilter" => "adbe.pkcs7.detached",
        // No /ByteRange at all, and opaque contents.
        "Contents" => Object::String(vec![0x42; 16], StringFormat::Hexadecimal),
    });
    let field_id = doc.add_object(dictionary! {
        "FT" => "Sig",
        "T" => Object::string_literal("Signature1"),
        "Subtype" => "Widget",
        "V" => signature_id,
    });
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .unwrap();
    doc.get_dictionary_mut(root).unwrap().set(
        "AcroForm",
        dictionary! { "Fields" => vec![Object::Reference(field_id)] },
    );
    let path = workspace.write("no-range.pdf", doc);

    let doc = pdf_core::Document::open(&path).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].intact);
    assert!(
        reports[0].reason.contains("/ByteRange"),
        "{}",
        reports[0].reason
    );
}

#[test]
fn an_unsigned_sig_field_is_reported_not_broken() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Empty", A4, Attributes::OnPage);
    let field_id = doc.add_object(dictionary! {
        "FT" => "Sig",
        "T" => Object::string_literal("SignatureSlot"),
        "Subtype" => "Widget",
    });
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .unwrap();
    doc.get_dictionary_mut(root).unwrap().set(
        "AcroForm",
        dictionary! { "Fields" => vec![Object::Reference(field_id)] },
    );
    let path = workspace.write("unsigned.pdf", doc);

    let doc = pdf_core::Document::open(&path).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].intact);
    assert!(
        reports[0].reason.contains("never signed"),
        "{}",
        reports[0].reason
    );
}

#[test]
fn an_orphan_signature_widget_is_still_found() {
    let workspace = Workspace::new();
    let mut doc = common::build(1, "Orphan", A4, Attributes::OnPage);
    // A signature widget only in /Annots, with no AcroForm at all.
    let annot_id = common::add_annotation(
        &mut doc,
        1,
        "Widget",
        [0, 0, 10, 10],
        common::Normal::None,
        0,
    );
    {
        let annot = doc.get_object_mut(annot_id).unwrap().as_dict_mut().unwrap();
        annot.set("FT", "Sig");
        annot.set(
            "V",
            Object::Dictionary(dictionary! {
                "Type" => "Sig",
                "Filter" => "Adobe.PPKLite",
                "SubFilter" => "adbe.pkcs7.detached",
                "Contents" => Object::String(vec![0x43; 8], StringFormat::Hexadecimal),
            }),
        );
    }
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .unwrap();
    doc.get_dictionary_mut(root).unwrap().remove(b"AcroForm");
    let path = workspace.write("orphan.pdf", doc);

    let doc = pdf_core::Document::open(&path).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert_eq!(reports.len(), 1, "the orphan widget is found: {reports:?}");
    assert!(!reports[0].intact);
}

#[test]
fn an_rsa_signed_document_verifies() {
    let workspace = Workspace::new();
    let path = signed_pdf(
        &workspace,
        "signed.pdf",
        "adbe.pkcs7.detached",
        Plan::Detached,
    );
    let doc = pdf_core::Document::open(&path).unwrap();

    let reports = verify_signatures(&doc).unwrap();
    assert_eq!(
        reports.len(),
        1,
        "a field in /Fields and /Annots is reported once"
    );

    let report = &reports[0];
    assert!(report.intact, "the report's reason: {}", report.reason);
    assert_eq!(report.name.as_deref(), Some("Signature1"));
    assert_eq!(report.sub_filter.as_deref(), Some("adbe.pkcs7.detached"));
    // The certificate's common name outranks the dictionary's /Name, which
    // happens to match.
    assert_eq!(report.signer.as_deref(), Some("Foolscap Fixture Signer"));
    assert_eq!(report.issuer.as_deref(), Some("Foolscap Fixture Signer"));
    assert_eq!(
        report.valid_from.as_deref(),
        Some("2026-01-01 00:00:00 UTC")
    );
    assert_eq!(
        report.valid_until.as_deref(),
        Some("2030-12-31 23:59:59 UTC")
    );
    assert_eq!(
        report.signed_at.as_deref(),
        Some("2026-10-24 00:00:00 +00:00")
    );
    assert_eq!(report.signing_reason.as_deref(), Some("fixture approval"));
    assert!(report
        .reason
        .contains("byte ranges hash to the signed SHA-256 digest"));
    assert!(
        report.reason.contains("signature verified (RSA-2048"),
        "{}",
        report.reason
    );
    assert!(report.reason.contains("chain-of-trust was NOT checked"));
}

#[test]
fn one_flipped_signed_byte_breaks_the_signature() {
    let workspace = Workspace::new();
    let path = signed_pdf(
        &workspace,
        "signed-tamper.pdf",
        "adbe.pkcs7.detached",
        Plan::Detached,
    );
    tamper(&path);

    let doc = pdf_core::Document::open(&path).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].intact);
    assert!(
        reports[0]
            .reason
            .contains("the file was modified after signing"),
        "{}",
        reports[0].reason
    );
}

#[test]
fn a_matching_digest_with_a_forged_signature_still_fails() {
    let workspace = Workspace::new();
    let path = signed_pdf(
        &workspace,
        "forged.pdf",
        "adbe.pkcs7.detached",
        Plan::Forged,
    );

    let doc = pdf_core::Document::open(&path).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].intact);
    // The digest matched — it is the signature that gave the forgery away.
    assert!(
        reports[0].reason.contains("signature does NOT verify"),
        "{}",
        reports[0].reason
    );
}

#[test]
fn a_legacy_sha1_signature_with_encapsulated_digest_verifies() {
    let workspace = Workspace::new();
    let path = signed_pdf(
        &workspace,
        "sha1.pdf",
        "adbe.pkcs7.sha1",
        Plan::Encapsulated,
    );

    let doc = pdf_core::Document::open(&path).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].intact,
        "the report's reason: {}",
        reports[0].reason
    );
    assert!(reports[0].reason.contains("encapsulated SHA-1 digest"));
    assert!(reports[0]
        .reason
        .contains("signature verified (RSA-2048 PKCS#1 v1.5, SHA-1)"));

    tamper(&path);
    let doc = pdf_core::Document::open(&path).unwrap();
    let reports = verify_signatures(&doc).unwrap();
    assert!(!reports[0].intact);
    assert!(
        reports[0].reason.contains("modified after signing"),
        "{}",
        reports[0].reason
    );
}
