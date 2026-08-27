//! The background thread that does everything slow.
//!
//! Two rules shape this module. MuPDF's context is thread-local, so the
//! renderer must live on, and never leave, one thread. And the GTK main loop
//! must never block, so every call into `pdf-core` happens here and comes back
//! as a message.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use pdf_core::render::{PageRenderer, RenderedPage, Scale};
use pdf_core::{CompressLevel, Document, PageRange, Progress};

/// Work the user interface asks for.
#[derive(Debug)]
pub enum Request {
    Open(PathBuf),
    /// Draw a page. `generation` lets stale results be discarded when the
    /// document has been replaced since the request went out.
    Render {
        page: usize,
        scale: Scale,
        generation: u64,
    },
    Save {
        path: PathBuf,
        edits: Edits,
    },
    Compress {
        path: PathBuf,
        level: CompressLevel,
        edits: Edits,
    },
    Ocr {
        path: PathBuf,
        language: String,
        edits: Edits,
    },
    ExportImages {
        directory: PathBuf,
        dpi: f32,
        edits: Edits,
    },
    Quit,
}

/// The pending changes to apply before writing anything out.
///
/// Edits are held here rather than applied to the file as they are made, so
/// that the document on disk is untouched until the user saves.
#[derive(Debug, Clone, Default)]
pub struct Edits {
    /// Original page indices, in the order they should appear.
    pub order: Vec<usize>,
    /// Extra rotation per original page index, in degrees.
    pub rotations: HashMap<usize, i32>,
}

impl Edits {
    /// True when applying these would change nothing.
    pub fn is_identity(&self, page_count: usize) -> bool {
        self.rotations.values().all(|degrees| degrees % 360 == 0)
            && self.order.len() == page_count
            && self.order.iter().enumerate().all(|(at, page)| at == *page)
    }
}

/// What the worker has to say.
#[derive(Debug)]
pub enum Response {
    Opened {
        path: PathBuf,
        page_count: usize,
    },
    Rendered {
        page: usize,
        generation: u64,
        image: Arc<RenderedPage>,
    },
    Progress(String),
    Finished {
        message: String,
    },
    Failed(String),
}

/// Start the worker thread and return the ends of its two channels.
pub fn spawn() -> (mpsc::Sender<Request>, async_channel::Receiver<Response>) {
    let (to_worker, requests) = mpsc::channel::<Request>();
    let (responses, from_worker) = async_channel::unbounded::<Response>();

    std::thread::Builder::new()
        .name("foolscap-worker".to_string())
        .spawn(move || run(requests, responses))
        .expect("could not start the worker thread");

    (to_worker, from_worker)
}

fn run(requests: mpsc::Receiver<Request>, responses: async_channel::Sender<Response>) {
    let mut renderer: Option<PageRenderer> = None;

    let say = |response: Response| {
        // A closed channel means the window has gone; there is nothing useful
        // left to do either way.
        let _ = responses.send_blocking(response);
    };

    while let Ok(request) = requests.recv() {
        match request {
            Request::Quit => break,

            Request::Open(path) => match PageRenderer::open(&path) {
                Ok(opened) => {
                    let page_count = opened.page_count();
                    renderer = Some(opened);
                    say(Response::Opened { path, page_count });
                }
                Err(error) => say(Response::Failed(error.to_string())),
            },

            Request::Render {
                page,
                scale,
                generation,
            } => {
                let Some(renderer) = renderer.as_ref() else {
                    continue;
                };
                match renderer.render(page, scale) {
                    Ok(image) => say(Response::Rendered {
                        page,
                        generation,
                        image,
                    }),
                    Err(error) => say(Response::Failed(error.to_string())),
                }
            }

            Request::Save { path, edits } => {
                let source = renderer.as_ref().map(|r| r.path().to_path_buf());
                match source {
                    Some(source) => match write_edited(&source, &path, &edits) {
                        Ok(()) => say(Response::Finished {
                            message: format!("Saved {}", path.display()),
                        }),
                        Err(error) => say(Response::Failed(error.to_string())),
                    },
                    None => say(Response::Failed("nothing is open".into())),
                }
            }

            Request::Compress { path, level, edits } => {
                let source = renderer.as_ref().map(|r| r.path().to_path_buf());
                let Some(source) = source else {
                    say(Response::Failed("nothing is open".into()));
                    continue;
                };

                let result = (|| -> pdf_core::Result<String> {
                    let staged = stage(&source, &edits)?;
                    let mut doc = staged.document;
                    let report = pdf_core::compress(&mut doc, level)?;
                    doc.save(&path)?;
                    Ok(format!(
                        "Compressed to {} ({:.0}% smaller)",
                        path.display(),
                        report.ratio_saved() * 100.0
                    ))
                })();

                match result {
                    Ok(message) => say(Response::Finished { message }),
                    Err(error) => say(Response::Failed(error.to_string())),
                }
            }

            Request::Ocr {
                path,
                language,
                edits,
            } => {
                let source = renderer.as_ref().map(|r| r.path().to_path_buf());
                let Some(source) = source else {
                    say(Response::Failed("nothing is open".into()));
                    continue;
                };

                let result = (|| -> pdf_core::Result<String> {
                    // Recognition works from a file, so the edited document has
                    // to exist on disk first.
                    let staged = stage(&source, &edits)?;
                    let intermediate = staged.write_temporary()?;

                    let options = pdf_core::OcrOptions {
                        language,
                        ..Default::default()
                    };

                    let mut tick = |progress: Progress| {
                        let _ = responses.send_blocking(Response::Progress(progress.message));
                    };

                    let report = pdf_core::ocr_with_progress(
                        intermediate.path(),
                        &path,
                        &options,
                        Some(&mut tick),
                    )?;

                    Ok(format!(
                        "Recognised {} page(s) into {}",
                        report.pages_recognised,
                        path.display()
                    ))
                })();

                match result {
                    Ok(message) => say(Response::Finished { message }),
                    Err(error) => say(Response::Failed(error.to_string())),
                }
            }

            Request::ExportImages {
                directory,
                dpi,
                edits,
            } => {
                let source = renderer.as_ref().map(|r| r.path().to_path_buf());
                let Some(source) = source else {
                    say(Response::Failed("nothing is open".into()));
                    continue;
                };

                let result = (|| -> pdf_core::Result<String> {
                    let staged = stage(&source, &edits)?;
                    let intermediate = staged.write_temporary()?;

                    let options = pdf_core::convert::PdfToImagesOptions {
                        dpi,
                        ..Default::default()
                    };

                    let written = pdf_core::convert::pdf_to_images(
                        intermediate.path(),
                        &directory,
                        &options,
                    )?;

                    Ok(format!(
                        "Exported {} image(s) to {}",
                        written.len(),
                        directory.display()
                    ))
                })();

                match result {
                    Ok(message) => say(Response::Finished { message }),
                    Err(error) => say(Response::Failed(error.to_string())),
                }
            }
        }
    }
}

/// A document with the pending edits applied, not yet written anywhere.
struct Staged {
    document: Document,
}

impl Staged {
    /// Write to a scratch file, for the operations that read from a path.
    fn write_temporary(mut self) -> pdf_core::Result<Temporary> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let path = std::env::temp_dir().join(format!(
            "foolscap-gui-{}-{}.pdf",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));

        self.document.save(&path)?;
        Ok(Temporary { path })
    }
}

struct Temporary {
    path: PathBuf,
}

impl Temporary {
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Apply the pending edits to a fresh copy of the document on disk.
fn stage(source: &std::path::Path, edits: &Edits) -> pdf_core::Result<Staged> {
    let mut doc = Document::open(source)?;

    // Nothing was changed, so there is nothing to rebuild. Rearranging a
    // document into the order it is already in would still rewrite every page
    // object for no gain.
    if edits.is_identity(doc.page_count()) {
        return Ok(Staged { document: doc });
    }

    // Rotations first, while page indices still refer to the original
    // document. Pages that share a rotation are done in one pass.
    let mut by_angle: HashMap<i32, Vec<usize>> = HashMap::new();
    for (page, degrees) in &edits.rotations {
        if degrees % 360 != 0 {
            by_angle.entry(*degrees).or_default().push(*page);
        }
    }

    for (degrees, mut pages) in by_angle {
        pages.sort_unstable();
        let segments = pages
            .into_iter()
            .map(|page| pdf_core::pages::Segment::Single(page + 1))
            .collect();
        pdf_core::rotate(&mut doc, &PageRange::Segments(segments), degrees)?;
    }

    let document = if edits.order.is_empty() {
        doc
    } else {
        pdf_core::arrange(doc, &edits.order)?
    };

    Ok(Staged { document })
}

fn write_edited(
    source: &std::path::Path,
    destination: &std::path::Path,
    edits: &Edits,
) -> pdf_core::Result<()> {
    let mut staged = stage(source, edits)?;
    staged.document.save(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untouched_document_has_no_edits_to_apply() {
        let edits = Edits {
            order: vec![0, 1, 2],
            rotations: HashMap::new(),
        };
        assert!(edits.is_identity(3));
    }

    #[test]
    fn a_reordered_document_has_edits() {
        let edits = Edits {
            order: vec![2, 1, 0],
            rotations: HashMap::new(),
        };
        assert!(!edits.is_identity(3));
    }

    #[test]
    fn a_deleted_page_counts_as_an_edit() {
        let edits = Edits {
            order: vec![0, 1],
            rotations: HashMap::new(),
        };
        assert!(!edits.is_identity(3));
    }

    #[test]
    fn a_full_turn_is_not_an_edit() {
        let mut rotations = HashMap::new();
        rotations.insert(0, 360);

        let edits = Edits {
            order: vec![0, 1],
            rotations,
        };
        assert!(edits.is_identity(2));
    }

    #[test]
    fn a_quarter_turn_is_an_edit() {
        let mut rotations = HashMap::new();
        rotations.insert(0, 90);

        let edits = Edits {
            order: vec![0, 1],
            rotations,
        };
        assert!(!edits.is_identity(2));
    }
}
