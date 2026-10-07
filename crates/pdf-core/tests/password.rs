//! Opening encrypted documents.
//!
//! The fixtures are encrypted by lopdf itself, in each of the schemes still
//! met in practice. What matters is the split between the three cases a user
//! sees: a file that opens without asking (empty user password), a file that
//! needs a password, and a password that is wrong.

mod common;

use std::path::{Path, PathBuf};

use common::{encrypt, Attributes, Cipher, Workspace, A4, CIPHERS};
use lopdf::{dictionary, Object};
use pdf_core::{Document, PdfError, SplitSpec};

const USER: &str = "user-secret";
const OWNER: &str = "owner-secret";

/// A three-page document labelled `Secret 1` to `Secret 3`, with a title, so
/// that both page content and the Info dictionary have to come back decrypted.
fn protected(workspace: &Workspace, name: &str, cipher: Cipher, user_password: &str) -> PathBuf {
    let mut doc = common::build(3, "Secret", A4, Attributes::OnPage);
    let info = doc.add_object(dictionary! {
        "Title" => Object::string_literal("Quarterly figures"),
    });
    doc.trailer.set("Info", Object::Reference(info));

    encrypt(&mut doc, cipher, user_password, OWNER);
    let path = workspace.write(name, doc);

    // Guard the fixture itself: the label must not be readable in the raw
    // bytes, or these tests would pass against a file that is not encrypted.
    let bytes = std::fs::read(&path).unwrap();
    assert!(common::declares_encryption(&path), "{cipher:?} fixture");
    assert!(
        !bytes.windows(8).any(|window| window == b"Secret 1"),
        "{cipher:?} fixture still holds plain text"
    );

    path
}

fn labels() -> Vec<String> {
    vec!["Secret 1".into(), "Secret 2".into(), "Secret 3".into()]
}

/// The text poppler extracts, or `None` when it is not installed. A second
/// opinion from a reader that is not the one that wrote the file.
fn pdftotext(path: &Path) -> Option<String> {
    match std::process::Command::new("pdftotext")
        .arg(path)
        .arg("-")
        .output()
    {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => panic!(
            "pdftotext failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(_) => {
            eprintln!("skipping the pdftotext check: poppler-utils is not installed");
            None
        }
    }
}

/// The fixtures are only worth testing against if another reader agrees they
/// are encrypted: poppler must refuse them without the password and read them
/// with it.
#[test]
fn the_fixtures_are_encrypted_as_far_as_poppler_is_concerned() {
    let workspace = Workspace::new();

    for cipher in CIPHERS {
        let path = protected(&workspace, &format!("check-{cipher:?}.pdf"), cipher, USER);

        let run = |password: Option<&str>| {
            let mut command = std::process::Command::new("pdftotext");
            if let Some(password) = password {
                command.arg("-upw").arg(password);
            }
            command.arg(&path).arg("-").output()
        };

        let Ok(without) = run(None) else {
            eprintln!("skipping the poppler fixture check: poppler-utils is not installed");
            return;
        };
        assert!(
            !without.status.success(),
            "{cipher:?}: opened with no password"
        );

        let with = run(Some(USER)).unwrap();
        assert!(with.status.success(), "{cipher:?}: {with:?}");
        let text = String::from_utf8_lossy(&with.stdout);
        assert!(text.contains("Secret 2"), "{cipher:?}: {text:?}");
    }
}

#[test]
fn an_empty_user_password_opens_without_asking() {
    let workspace = Workspace::new();

    for cipher in CIPHERS {
        let path = protected(&workspace, &format!("open-{cipher:?}.pdf"), cipher, "");

        let doc = Document::open(&path).unwrap_or_else(|error| panic!("{cipher:?}: {error}"));
        assert_eq!(doc.page_count(), 3, "{cipher:?}");
        assert!(doc.was_encrypted(), "{cipher:?}");
        assert_eq!(
            doc.metadata().unwrap().title.as_deref(),
            Some("Quarterly figures"),
            "{cipher:?}"
        );
    }
}

#[test]
fn a_password_protected_file_needs_a_password() {
    let workspace = Workspace::new();

    for cipher in CIPHERS {
        let path = protected(&workspace, &format!("locked-{cipher:?}.pdf"), cipher, USER);

        let error = Document::open(&path).unwrap_err();
        assert!(
            matches!(&error, PdfError::Encrypted(p) if p == &path),
            "{cipher:?}: got {error:?}"
        );
    }
}

#[test]
fn a_wrong_password_is_reported_as_wrong() {
    let workspace = Workspace::new();

    for cipher in CIPHERS {
        let path = protected(&workspace, &format!("wrong-{cipher:?}.pdf"), cipher, USER);

        let error = Document::open_with_password(&path, "not-it").unwrap_err();
        assert!(
            matches!(&error, PdfError::WrongPassword(p) if p == &path),
            "{cipher:?}: got {error:?}"
        );
    }
}

#[test]
fn the_user_password_opens_the_document() {
    let workspace = Workspace::new();

    for cipher in CIPHERS {
        let path = protected(&workspace, &format!("user-{cipher:?}.pdf"), cipher, USER);

        let doc = Document::open_with_password(&path, USER)
            .unwrap_or_else(|error| panic!("{cipher:?}: {error}"));
        assert_eq!(doc.page_count(), 3, "{cipher:?}");
        assert!(doc.was_encrypted(), "{cipher:?}");
        assert_eq!(
            doc.metadata().unwrap().title.as_deref(),
            Some("Quarterly figures"),
            "{cipher:?}"
        );
    }
}

#[test]
fn the_owner_password_opens_the_document_too() {
    let workspace = Workspace::new();

    for cipher in CIPHERS {
        let path = protected(&workspace, &format!("owner-{cipher:?}.pdf"), cipher, USER);

        let doc = Document::open_with_password(&path, OWNER)
            .unwrap_or_else(|error| panic!("{cipher:?}: {error}"));
        assert_eq!(doc.page_count(), 3, "{cipher:?}");
    }
}

#[test]
fn saving_writes_an_unencrypted_file() {
    let workspace = Workspace::new();

    for cipher in CIPHERS {
        let path = protected(&workspace, &format!("in-{cipher:?}.pdf"), cipher, USER);
        let out = workspace.join(&format!("out-{cipher:?}.pdf"));

        Document::open_with_password(&path, USER)
            .unwrap()
            .save(&out)
            .unwrap();

        assert!(!common::declares_encryption(&out), "{cipher:?}");

        // Reopens with no password at all, and is no longer marked encrypted.
        let reopened = Document::open(&out).unwrap();
        assert_eq!(reopened.page_count(), 3, "{cipher:?}");
        assert!(!reopened.was_encrypted(), "{cipher:?}");
        assert_eq!(
            reopened.metadata().unwrap().title.as_deref(),
            Some("Quarterly figures"),
            "{cipher:?}"
        );

        // The content streams are plain text again.
        assert_eq!(common::page_labels(&out), labels(), "{cipher:?}");

        if let Some(text) = pdftotext(&out) {
            for label in labels() {
                assert!(text.contains(&label), "{cipher:?}: {label} not in {text:?}");
            }
        }
    }
}

#[test]
fn an_empty_user_password_file_loses_its_restrictions_when_saved() {
    let workspace = Workspace::new();
    let path = protected(&workspace, "restricted.pdf", Cipher::Aes256, "");
    let out = workspace.join("out.pdf");

    Document::open(&path).unwrap().save(&out).unwrap();

    assert!(!common::declares_encryption(&out));
    assert_eq!(common::page_labels(&out), labels());
}

#[test]
fn a_password_is_ignored_by_a_file_that_does_not_need_one() {
    let workspace = Workspace::new();
    let plain = workspace.document("plain.pdf", 2, "Page");

    let doc = Document::open_with_password(&plain, "anything").unwrap();
    assert_eq!(doc.page_count(), 2);
    assert!(!doc.was_encrypted());
}

#[test]
fn merging_offers_the_password_to_every_input() {
    let workspace = Workspace::new();
    let plain = workspace.document("plain.pdf", 2, "Page");
    let locked = protected(&workspace, "locked.pdf", Cipher::Aes128, USER);
    let out = workspace.join("merged.pdf");
    let inputs = vec![plain, locked.clone()];

    let error = pdf_core::merge(&inputs, &out).unwrap_err();
    assert!(
        matches!(&error, PdfError::Encrypted(p) if p == &locked),
        "got {error:?}"
    );
    assert!(!out.exists(), "a failed merge must not write anything");

    pdf_core::merge_with_progress(&inputs, &out, Some(USER), None).unwrap();

    assert!(!common::declares_encryption(&out));
    let mut expected = vec!["Page 1".to_string(), "Page 2".to_string()];
    expected.extend(labels());
    assert_eq!(common::page_labels(&out), expected);
}

#[test]
fn splitting_a_protected_document_writes_plain_pieces() {
    let workspace = Workspace::new();
    let locked = protected(&workspace, "locked.pdf", Cipher::Rc4, USER);
    let out_dir = workspace.join("pieces");
    let spec = SplitSpec::Every(1);

    let error = pdf_core::split_plan(&locked, &spec, &out_dir, None).unwrap_err();
    assert!(matches!(error, PdfError::Encrypted(_)), "got {error:?}");

    let planned = pdf_core::split_plan(&locked, &spec, &out_dir, Some(USER)).unwrap();
    let written =
        pdf_core::split_with_progress(&locked, &spec, &out_dir, Some(USER), None).unwrap();
    assert_eq!(planned, written);
    assert_eq!(written.len(), 3);

    for (path, label) in written.iter().zip(labels()) {
        assert!(!common::declares_encryption(path));
        assert_eq!(common::page_labels(path), vec![label]);
    }
}

#[cfg(feature = "render")]
mod rendering {
    use super::*;
    use pdf_core::render::PageRenderer;

    #[test]
    fn an_empty_user_password_renders_without_asking() {
        let workspace = Workspace::new();

        for cipher in CIPHERS {
            let path = protected(&workspace, &format!("open-{cipher:?}.pdf"), cipher, "");

            let renderer =
                PageRenderer::open(&path).unwrap_or_else(|error| panic!("{cipher:?}: {error}"));
            assert_eq!(renderer.page_count(), 3, "{cipher:?}");
            assert!(
                renderer.page_text(0).unwrap().contains("Secret 1"),
                "{cipher:?}"
            );
        }
    }

    #[test]
    fn the_renderer_tells_a_missing_password_from_a_wrong_one() {
        let workspace = Workspace::new();

        for cipher in CIPHERS {
            let path = protected(&workspace, &format!("locked-{cipher:?}.pdf"), cipher, USER);

            let missing = PageRenderer::open(&path).unwrap_err();
            assert!(
                matches!(missing, PdfError::Encrypted(_)),
                "{cipher:?}: got {missing:?}"
            );

            let wrong = PageRenderer::open_with_password(&path, "not-it").unwrap_err();
            assert!(
                matches!(wrong, PdfError::WrongPassword(_)),
                "{cipher:?}: got {wrong:?}"
            );
        }
    }

    #[test]
    fn the_renderer_reads_a_protected_page_with_either_password() {
        let workspace = Workspace::new();

        for cipher in CIPHERS {
            let path = protected(&workspace, &format!("locked-{cipher:?}.pdf"), cipher, USER);

            for password in [USER, OWNER] {
                let renderer = PageRenderer::open_with_password(&path, password)
                    .unwrap_or_else(|error| panic!("{cipher:?}/{password}: {error}"));
                assert_eq!(renderer.page_count(), 3);
                assert!(
                    renderer.page_text(2).unwrap().contains("Secret 3"),
                    "{cipher:?}/{password}"
                );
            }
        }
    }

    #[test]
    fn a_decrypted_copy_has_the_same_text_as_the_original() {
        let workspace = Workspace::new();
        let path = protected(&workspace, "locked.pdf", Cipher::Aes256, USER);
        let out = workspace.join("plain.pdf");

        Document::open_with_password(&path, USER)
            .unwrap()
            .save(&out)
            .unwrap();

        let original = PageRenderer::open_with_password(&path, USER).unwrap();
        let copy = PageRenderer::open(&out).unwrap();
        for page in 0..3 {
            assert_eq!(
                original.page_text(page).unwrap(),
                copy.page_text(page).unwrap()
            );
        }
    }
}
