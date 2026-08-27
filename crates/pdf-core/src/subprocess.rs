//! Running external tools.
//!
//! Foolscap shells out to LibreOffice and Tesseract rather than linking them.
//! Both need the same care — find the binary, bound how long it may run,
//! capture what it said when it failed — so that lives here once.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::error::{PdfError, Result};

/// How often to check whether a child has finished. Short enough that a fast
/// tool is not held up, long enough not to spin.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// An external program Foolscap knows how to find and talk about.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tool {
    /// What to call it when reporting to the user.
    pub name: &'static str,
    /// Executable names to try, in order of preference.
    pub binaries: &'static [&'static str],
    /// What to tell the user to run when it is missing.
    pub install: &'static str,
}

impl Tool {
    /// The path to this tool, or an error naming the package that provides it.
    pub(crate) fn locate(&self) -> Result<PathBuf> {
        self.find().ok_or(PdfError::ToolMissing {
            tool: self.name,
            install: self.install,
        })
    }

    /// Whether the tool is available, without treating absence as an error.
    /// The CLI uses this to report capabilities honestly.
    pub(crate) fn is_available(&self) -> bool {
        self.find().is_some()
    }

    fn find(&self) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;

        for binary in self.binaries {
            for directory in std::env::split_paths(&path) {
                let candidate = directory.join(binary);
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }

        None
    }

    /// Run the tool, failing if it takes longer than `timeout` or exits
    /// non-zero.
    pub(crate) fn run(&self, args: &[String], timeout: Duration) -> Result<Output> {
        let program = self.locate()?;

        tracing::debug!(tool = self.name, ?args, "running external tool");

        let mut child = Command::new(&program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| PdfError::Io {
                path: program.clone(),
                source,
            })?;

        let started = Instant::now();

        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if started.elapsed() >= timeout {
                        // The child is wedged. Killing it is the whole point of
                        // having a timeout, so ignore a failure to reap.
                        let _ = child.kill();
                        let _ = child.wait();

                        return Err(PdfError::ToolTimeout {
                            tool: self.name,
                            seconds: timeout.as_secs(),
                        });
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(source) => {
                    return Err(PdfError::Io {
                        path: program,
                        source,
                    })
                }
            }
        }

        let output = child.wait_with_output().map_err(|source| PdfError::Io {
            path: program,
            source,
        })?;

        if !output.status.success() {
            return Err(PdfError::ToolFailed {
                tool: self.name,
                status: output.status.code(),
                message: first_meaningful_line(&output.stderr, &output.stdout),
            });
        }

        Ok(output)
    }
}

/// The most useful line of a failed tool's output.
///
/// Tools are chatty on failure and most of it is noise; the first non-empty
/// line of stderr is almost always the actual complaint.
fn first_meaningful_line(stderr: &[u8], stdout: &[u8]) -> String {
    for stream in [stderr, stdout] {
        let text = String::from_utf8_lossy(stream);
        if let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) {
            return line.to_string();
        }
    }

    "no output".to_string()
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// LibreOffice, for converting to and from office formats.
pub(crate) const LIBREOFFICE: Tool = Tool {
    name: "LibreOffice",
    binaries: &["soffice", "libreoffice"],
    install: "sudo apt install libreoffice",
};

/// Tesseract, for optical character recognition.
#[cfg(feature = "ocr")]
pub(crate) const TESSERACT: Tool = Tool {
    name: "Tesseract",
    binaries: &["tesseract"],
    install: "sudo apt install tesseract-ocr tesseract-ocr-eng",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_tool_names_its_package() {
        let tool = Tool {
            name: "Nonexistent",
            binaries: &["definitely-not-a-real-binary-name-42"],
            install: "sudo apt install nothing",
        };

        assert!(!tool.is_available());

        let error = tool.locate().unwrap_err();
        assert!(
            matches!(error, PdfError::ToolMissing { install, .. } if install == "sudo apt install nothing"),
            "got {error:?}"
        );
    }

    #[test]
    fn a_tool_on_the_path_is_found() {
        // `sh` is on PATH on every system this targets.
        let tool = Tool {
            name: "Shell",
            binaries: &["sh"],
            install: "impossible",
        };

        assert!(tool.is_available());
        assert!(tool.locate().unwrap().is_absolute());
    }

    #[test]
    fn the_first_listed_binary_wins() {
        let tool = Tool {
            name: "Shell",
            binaries: &["definitely-not-real-99", "sh"],
            install: "impossible",
        };

        assert!(tool.locate().unwrap().ends_with("sh"));
    }

    #[test]
    fn a_nonzero_exit_carries_the_message() {
        let tool = Tool {
            name: "Shell",
            binaries: &["sh"],
            install: "impossible",
        };

        let error = tool
            .run(
                &["-c".into(), "echo something broke >&2; exit 3".into()],
                Duration::from_secs(10),
            )
            .unwrap_err();

        match error {
            PdfError::ToolFailed {
                status, message, ..
            } => {
                assert_eq!(status, Some(3));
                assert_eq!(message, "something broke");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_wedged_tool_is_killed() {
        let tool = Tool {
            name: "Shell",
            binaries: &["sh"],
            install: "impossible",
        };

        let error = tool
            .run(
                &["-c".into(), "sleep 30".into()],
                Duration::from_millis(200),
            )
            .unwrap_err();

        assert!(
            matches!(error, PdfError::ToolTimeout { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn output_is_captured_on_success() {
        let tool = Tool {
            name: "Shell",
            binaries: &["sh"],
            install: "impossible",
        };

        let output = tool
            .run(&["-c".into(), "echo hello".into()], Duration::from_secs(10))
            .unwrap();

        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    fn stdout_is_used_when_stderr_is_empty() {
        assert_eq!(
            first_meaningful_line(b"", b"  \nfrom stdout\n"),
            "from stdout"
        );
        assert_eq!(
            first_meaningful_line(b"from stderr", b"from stdout"),
            "from stderr"
        );
        assert_eq!(first_meaningful_line(b"", b""), "no output");
    }
}
