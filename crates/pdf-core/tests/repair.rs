//! `repair` on password-protected input.
//!
//! Each test encrypts a generated document, damages the bytes the way a bad
//! download or a crashed writer would, and checks what comes back out.

mod common;

use std::path::PathBuf;

use common::{Attributes, Cipher, Workspace, A4, CIPHERS};
use pdf_core::{Document, PdfError};

const USER: &str = "open sesame";
const OWNER: &str = "owner";

fn locked_bytes(cipher: Cipher, user: &str) -> Vec<u8> {
    let mut doc = common::build(2, "Secret", A4, Attributes::OnPage);
    common::encrypt(&mut doc, cipher, user, OWNER);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("could not write fixture");
    bytes
}

fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes.windows(needle.len()).position(|w| w == needle)
}

fn rfind(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes.windows(needle.len()).rposition(|w| w == needle)
}

/// Make every direct stream `/Length` wrong, keeping its digit count so that
/// nothing after it moves.
fn break_stream_lengths(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let mut changed = 0;
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], b"stream").map(|at| at + from) {
        from = at + 6;
        // Only the keyword that opens data: it is followed by a line end.
        if !matches!(bytes.get(at + 6), Some(b'\r' | b'\n'))
            || (at >= 3 && &bytes[at - 3..at] == b"end")
        {
            continue;
        }
        let Some(key) = rfind(&bytes[..at], b"/Length ") else {
            continue;
        };
        let digits_start = key + b"/Length ".len();
        let digits = bytes[digits_start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        // Stay inside the same object: the dictionary is right before `stream`.
        if digits == 0 || at - digits_start > 400 {
            continue;
        }
        for byte in &mut out[digits_start..digits_start + digits] {
            *byte = b'9';
        }
        changed += 1;
    }
    assert!(changed > 0, "the fixture had no stream to damage");
    out
}

/// Cut the file off before its `startxref`, as an interrupted write would.
fn drop_the_tail(bytes: &[u8]) -> Vec<u8> {
    let at = rfind(bytes, b"startxref").expect("fixture has a startxref");
    bytes[..at].to_vec()
}

/// Point `startxref` at nonsense.
fn bad_startxref(bytes: &[u8]) -> Vec<u8> {
    let at = rfind(bytes, b"startxref").expect("fixture has a startxref") + b"startxref\n".len();
    let digits = bytes[at..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    let mut out = bytes[..at].to_vec();
    out.extend_from_slice(&vec![b'7'; digits]);
    out.extend_from_slice(&bytes[at + digits..]);
    out
}

type Damage = (&'static str, fn(&[u8]) -> Vec<u8>);

const DAMAGE: [Damage; 3] = [
    ("wrong stream lengths", break_stream_lengths),
    ("missing tail", drop_the_tail),
    ("bad startxref", bad_startxref),
];

fn write(workspace: &Workspace, name: &str, bytes: &[u8]) -> PathBuf {
    let path = workspace.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn assert_plain_and_intact(path: &std::path::Path) {
    assert!(
        !common::declares_encryption(path),
        "the repaired file should not be encrypted"
    );
    // Opens with no password at all, and the pages kept their content.
    let doc = Document::open(path).expect("repaired file should open without a password");
    assert_eq!(doc.page_count(), 2);
    assert_eq!(common::page_labels(path), ["Secret 1", "Secret 2"]);
    if let Some(text) = common::pdftotext(path) {
        assert!(
            text.contains("Secret 1") && text.contains("Secret 2"),
            "{text:?}"
        );
    }
}

#[test]
fn the_damage_really_changes_the_file() {
    // Guards against a fixture helper quietly doing nothing.
    let bytes = locked_bytes(Cipher::Aes128, USER);
    for (name, damage) in DAMAGE {
        assert_ne!(damage(&bytes), bytes, "{name}");
    }
}

#[test]
fn an_intact_locked_file_repairs_cleanly_with_its_password() {
    for cipher in CIPHERS {
        let workspace = Workspace::new();
        let input = write(&workspace, "in.pdf", &locked_bytes(cipher, USER));
        let out = workspace.join("out.pdf");

        let report = pdf_core::repair_with(&input, &out, Some(USER)).unwrap();
        assert!(report.is_clean(), "{cipher:?}: {report:?}");
        assert_plain_and_intact(&out);
    }
}

#[test]
fn a_damaged_locked_file_is_repaired_with_the_right_password() {
    for cipher in CIPHERS {
        let bytes = locked_bytes(cipher, USER);
        for (name, damage) in DAMAGE {
            let workspace = Workspace::new();
            let input = write(&workspace, "in.pdf", &damage(&bytes));
            let out = workspace.join("out.pdf");

            let report = pdf_core::repair_with(&input, &out, Some(USER))
                .unwrap_or_else(|error| panic!("{cipher:?} / {name}: {error:?}"));
            assert!(
                !report.is_clean(),
                "{cipher:?} / {name}: nothing was reported"
            );
            assert_plain_and_intact(&out);
        }
    }
}

#[test]
fn the_owner_password_works_too() {
    let workspace = Workspace::new();
    let bytes = break_stream_lengths(&locked_bytes(Cipher::Aes256, USER));
    let input = write(&workspace, "in.pdf", &bytes);
    let out = workspace.join("out.pdf");

    pdf_core::repair_with(&input, &out, Some(OWNER)).unwrap();
    assert_plain_and_intact(&out);
}

#[test]
fn a_damaged_locked_file_without_a_password_says_it_is_encrypted() {
    for cipher in CIPHERS {
        let bytes = locked_bytes(cipher, USER);
        for (name, damage) in DAMAGE {
            let workspace = Workspace::new();
            let input = write(&workspace, "in.pdf", &damage(&bytes));
            let out = workspace.join("out.pdf");

            let short = pdf_core::repair(&input, &out).unwrap_err();
            assert!(
                matches!(short, PdfError::Encrypted(_)),
                "{cipher:?} / {name}: {short:?}"
            );
            let full = pdf_core::repair_with(&input, &out, None).unwrap_err();
            assert!(
                matches!(full, PdfError::Encrypted(_)),
                "{cipher:?} / {name}: {full:?}"
            );
            assert!(!out.exists(), "{cipher:?} / {name}: wrote output anyway");
        }
    }
}

#[test]
fn a_damaged_locked_file_with_the_wrong_password_says_so() {
    for cipher in CIPHERS {
        let bytes = locked_bytes(cipher, USER);
        for (name, damage) in DAMAGE {
            let workspace = Workspace::new();
            let input = write(&workspace, "in.pdf", &damage(&bytes));
            let out = workspace.join("out.pdf");

            let error = pdf_core::repair_with(&input, &out, Some("not it")).unwrap_err();
            assert!(
                matches!(error, PdfError::WrongPassword(_)),
                "{cipher:?} / {name}: {error:?}"
            );
            assert!(!out.exists(), "{cipher:?} / {name}: wrote output anyway");
        }
    }
}

#[test]
fn a_file_that_opens_without_a_password_repairs_without_one() {
    // Restricted by an owner password only: readable by anyone.
    for cipher in CIPHERS {
        let bytes = locked_bytes(cipher, "");
        for (name, damage) in DAMAGE {
            let workspace = Workspace::new();
            let input = write(&workspace, "in.pdf", &damage(&bytes));
            let out = workspace.join("out.pdf");

            pdf_core::repair(&input, &out)
                .unwrap_or_else(|error| panic!("{cipher:?} / {name}: {error:?}"));
            assert_plain_and_intact(&out);
        }
    }
}

#[test]
fn a_password_given_for_a_plain_damaged_file_is_ignored() {
    let workspace = Workspace::new();
    let plain = workspace.document("plain.pdf", 2, "Secret");
    let bytes = break_stream_lengths(&std::fs::read(&plain).unwrap());
    let input = write(&workspace, "in.pdf", &bytes);
    let out = workspace.join("out.pdf");

    pdf_core::repair_with(&input, &out, Some("unneeded")).unwrap();
    assert_plain_and_intact(&out);
}
