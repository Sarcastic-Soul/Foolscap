//! Runs the real `foolscap` binary. Every subcommand gets a `--help` smoke
//! test; the real runs build their input with `from-text`, so they need the
//! `convert` feature but no external tool.

use std::process::{Command, Output};

use clap::CommandFactory;
use foolscap_cli::Cli;

fn foolscap(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_foolscap"))
        .args(args)
        .env_remove("FOOLSCAP_PASSWORD")
        .output()
        .expect("the foolscap binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn every_subcommand_answers_help() {
    for command in Cli::command().get_subcommands() {
        let name = command.get_name();
        let out = foolscap(&[name, "--help"]);
        assert!(
            out.status.success(),
            "{name} --help failed: {}",
            text(&out.stderr)
        );
        assert!(
            text(&out.stdout).contains("Usage"),
            "{name} --help printed no usage"
        );
    }
}

#[test]
fn the_new_subcommands_exist() {
    let names: Vec<_> = Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    for wanted in ["flatten", "repair", "strip"] {
        assert!(
            names.iter().any(|n| n == wanted),
            "{wanted} is not wired in"
        );
    }
}

#[cfg(feature = "convert")]
mod real {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn ok(args: &[&str]) -> String {
        let out = foolscap(args);
        assert!(
            out.status.success(),
            "foolscap {args:?} failed: {}",
            text(&out.stderr)
        );
        text(&out.stdout)
    }

    fn s(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    /// A three-page PDF made by `from-text`, plus the directory holding it.
    fn sample() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("in.txt");
        let body: String = (0..400).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&input, body).unwrap();
        let pdf = dir.path().join("sample.pdf");
        ok(&["from-text", s(&input), "-o", s(&pdf), "--font-size", "24"]);
        (dir, pdf)
    }

    #[test]
    fn writing_commands_refuse_to_overwrite_without_force() {
        let (dir, pdf) = sample();
        let out = dir.path().join("out.pdf");
        std::fs::write(&out, b"existing").unwrap();

        for command in ["strip", "flatten", "repair"] {
            let result = foolscap(&[command, s(&pdf), "-o", s(&out)]);
            assert!(!result.status.success(), "{command} overwrote a file");
            assert!(
                text(&result.stderr).contains("--force"),
                "{command}: {}",
                text(&result.stderr)
            );
        }
        assert_eq!(std::fs::read(&out).unwrap(), b"existing");

        ok(&["strip", s(&pdf), "-o", s(&out), "--force"]);
        assert_ne!(std::fs::read(&out).unwrap(), b"existing");
    }

    #[test]
    fn strip_removes_metadata() {
        let (dir, pdf) = sample();
        let titled = dir.path().join("titled.pdf");
        ok(&["meta", s(&pdf), "--set-title", "Secret", "-o", s(&titled)]);
        assert!(ok(&["info", s(&titled)]).contains("Secret"));

        let clean = dir.path().join("clean.pdf");
        ok(&["strip", s(&titled), "-o", s(&clean)]);
        assert!(!ok(&["info", s(&clean)]).contains("Secret"));
    }

    #[test]
    fn flatten_and_repair_produce_a_readable_file() {
        let (dir, pdf) = sample();

        let flat = dir.path().join("flat.pdf");
        let said = ok(&["flatten", s(&pdf), "-o", s(&flat)]);
        assert!(said.contains("flattened"), "{said}");
        ok(&["info", s(&flat)]);

        let fixed = dir.path().join("fixed.pdf");
        ok(&["repair", s(&pdf), "-o", s(&fixed)]);
        ok(&["info", s(&fixed)]);
    }

    #[test]
    fn flatten_rejects_conflicting_flags() {
        let (dir, pdf) = sample();
        let out = dir.path().join("o.pdf");
        let result = foolscap(&[
            "flatten",
            s(&pdf),
            "--forms-only",
            "--annots-only",
            "-o",
            s(&out),
        ]);
        assert!(!result.status.success());
        assert!(!out.exists());
    }

    #[test]
    fn encrypted_files_need_the_password() {
        let (dir, pdf) = sample();
        let locked = dir.path().join("locked.pdf");
        ok(&[
            "encrypt",
            s(&pdf),
            "--user-password",
            "pw",
            "-o",
            s(&locked),
        ]);

        let blocked = foolscap(&["strip", s(&locked), "-o", s(&dir.path().join("a.pdf"))]);
        assert!(!blocked.status.success());
        assert!(text(&blocked.stderr).contains("--password"));

        let clean = dir.path().join("b.pdf");
        ok(&["--password", "pw", "strip", s(&locked), "-o", s(&clean)]);
        ok(&["info", s(&clean)]);
    }

    #[test]
    fn text_and_markdown_become_pdfs() {
        let dir = TempDir::new().unwrap();
        let md = dir.path().join("a.md");
        std::fs::write(&md, "# Title\n\nSome *words* here.\n").unwrap();
        let pdf = dir.path().join("a.pdf");
        ok(&["from-markdown", s(&md), "-o", s(&pdf)]);
        assert!(ok(&["info", s(&pdf)]).contains("1"));
    }

    #[cfg(feature = "render")]
    #[test]
    fn pages_go_to_html_and_zip() {
        let (dir, pdf) = sample();

        let html = dir.path().join("o.html");
        ok(&["to-html", s(&pdf), "-o", s(&html)]);
        assert!(std::fs::read_to_string(&html).unwrap().contains("line 0"));

        let zip = dir.path().join("o.zip");
        ok(&["to-zip", s(&pdf), "--dpi", "40", "-o", s(&zip)]);
        assert!(std::fs::metadata(&zip).unwrap().len() > 0);

        let again = foolscap(&["to-zip", s(&pdf), "-o", s(&zip)]);
        assert!(!again.status.success());
    }
}
