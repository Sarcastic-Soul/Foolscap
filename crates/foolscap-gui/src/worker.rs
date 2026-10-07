//! The background thread that does everything slow.
//!
//! Two rules shape this module. MuPDF's context is thread-local, so the
//! renderer must live on, and never leave, one thread. And the GTK main loop
//! must never block, so every call into `pdf-core` happens here and comes back
//! as a message.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use pdf_core::render::{PageRenderer, RenderedPage, Scale};
use pdf_core::{CompressLevel, Document, PageRange, PdfError, Progress};

/// Resolution limits for the main view, so that zooming or a very wide window
/// cannot ask MuPDF for an enormous bitmap.
const MIN_VIEW_DPI: f32 = 18.0;
const MAX_VIEW_DPI: f32 = 600.0;

/// Work the user interface asks for.
#[derive(Debug)]
pub enum Request {
    Open(PathBuf),
    /// Draw a page. `generation` lets stale results be discarded when the
    /// document has been replaced since the request went out.
    Render {
        page: usize,
        size: Size,
        purpose: Purpose,
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
    /// Combine several files into `output`, then open it.
    Merge {
        inputs: Vec<PathBuf>,
        output: PathBuf,
    },
    Quit,
}

/// How big to draw a page.
#[derive(Debug, Clone, Copy)]
pub enum Size {
    /// Fit inside a square of this many pixels.
    Box(u32),
    Dpi(f32),
    /// As wide as this many pixels once the page's pending rotation is applied.
    Width {
        pixels: u32,
        quarter_turned: bool,
    },
}

/// Where a rendered page is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Thumbnail,
    /// The main view. The ticket identifies the request, so that a slow render
    /// for an old zoom level cannot replace a newer one.
    View {
        ticket: u64,
    },
}

/// The operation a failure belongs to, so the window can decide how loudly to
/// report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Open,
    Render,
    Save,
    Compress,
    Ocr,
    Export,
    Merge,
}

/// The pending changes to apply before writing anything out.
///
/// Edits are held here rather than applied to the file as they are made, so
/// that the document on disk is untouched until the user saves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
        purpose: Purpose,
        /// The resolution used, which sets the main view's zoom level.
        dpi: f32,
        image: Arc<RenderedPage>,
    },
    Progress(String),
    /// The open file is about to be overwritten, so pages are now read from a
    /// private copy of it instead.
    SourceMoved {
        from: PathBuf,
        to: PathBuf,
    },
    Saved {
        path: PathBuf,
    },
    Finished {
        message: String,
    },
    Failed {
        task: Task,
        error: PdfError,
    },
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
    // Private copies of files that were overwritten while open. They live as
    // long as the window, since undo may still need their pages.
    let mut copies: Vec<Temporary> = Vec::new();

    let say = |response: Response| {
        // A closed channel means the window has gone; there is nothing useful
        // left to do either way.
        let _ = responses.send_blocking(response);
    };
    let fail = |task: Task, error: PdfError| say(Response::Failed { task, error });
    let tick = |progress: Progress| say(Response::Progress(progress.message));

    while let Ok(request) = requests.recv() {
        match request {
            Request::Quit => break,

            Request::Open(path) => match PageRenderer::open(&path) {
                Ok(opened) => {
                    let page_count = opened.page_count();
                    renderer = Some(opened);
                    say(Response::Opened { path, page_count });
                }
                Err(error) => fail(Task::Open, error),
            },

            Request::Render {
                page,
                size,
                purpose,
                generation,
            } => {
                let Some(renderer) = renderer.as_ref() else {
                    continue;
                };

                match render(renderer, page, size) {
                    Ok((dpi, image)) => say(Response::Rendered {
                        page,
                        generation,
                        purpose,
                        dpi,
                        image,
                    }),
                    Err(error) => fail(Task::Render, error),
                }
            }

            Request::Save { path, edits } => {
                let result = protect_source(&mut renderer, &mut copies, &path, &say)
                    .and_then(|source| write_edited(&source, &path, &edits));

                match result {
                    Ok(()) => say(Response::Saved { path }),
                    Err(error) => fail(Task::Save, error),
                }
            }

            Request::Compress { path, level, edits } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(&mut renderer, &mut copies, &path, &say)?;
                    let mut doc = stage(&source, &edits)?.document;
                    let report = pdf_core::compress(&mut doc, level)?;
                    save_atomically(&mut doc, &path)?;
                    Ok(format!(
                        "Compressed to {} ({:.0}% smaller)",
                        path.display(),
                        report.ratio_saved() * 100.0
                    ))
                })();

                match result {
                    Ok(message) => say(Response::Finished { message }),
                    Err(error) => fail(Task::Compress, error),
                }
            }

            Request::Ocr {
                path,
                language,
                edits,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(&mut renderer, &mut copies, &path, &say)?;

                    // Recognition works from a file, so the edited document has
                    // to exist on disk first.
                    let intermediate = stage(&source, &edits)?.write_temporary()?;

                    let options = pdf_core::OcrOptions {
                        language,
                        ..Default::default()
                    };

                    let mut tick = tick;
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
                    Err(error) => fail(Task::Ocr, error),
                }
            }

            Request::ExportImages {
                directory,
                dpi,
                edits,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = current_source(&renderer)?;
                    let intermediate = stage(&source, &edits)?.write_temporary()?;

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
                    Err(error) => fail(Task::Export, error),
                }
            }

            Request::Merge { inputs, output } => {
                let result = (|| -> pdf_core::Result<PageRenderer> {
                    // Merging over the open file is allowed; the private copy
                    // keeps the way back to it for undo.
                    if renderer.is_some() {
                        protect_source(&mut renderer, &mut copies, &output, &say)?;
                    }

                    let scratch = Temporary::beside(&output);
                    let mut tick = tick;
                    pdf_core::merge_with_progress(&inputs, scratch.path(), None, Some(&mut tick))?;
                    scratch.persist(&output)?;

                    PageRenderer::open(&output)
                })();

                match result {
                    Ok(opened) => {
                        let page_count = opened.page_count();
                        renderer = Some(opened);
                        say(Response::Opened {
                            path: output,
                            page_count,
                        });
                    }
                    Err(error) => fail(Task::Merge, error),
                }
            }
        }
    }
}

/// Draw one page, returning the resolution used along with the pixels.
fn render(
    renderer: &PageRenderer,
    page: usize,
    size: Size,
) -> pdf_core::Result<(f32, Arc<RenderedPage>)> {
    let (scale, dpi) = match size {
        Size::Box(edge) => (Scale::FitBox(edge), 0.0),
        Size::Dpi(dpi) => {
            let dpi = dpi.clamp(MIN_VIEW_DPI, MAX_VIEW_DPI);
            (Scale::Dpi(dpi), dpi)
        }
        Size::Width {
            pixels,
            quarter_turned,
        } => {
            let (width, height) = renderer.page_size(page)?;
            let across = if quarter_turned { height } else { width };
            let dpi = if across > 0.0 {
                pixels as f32 * 72.0 / across
            } else {
                96.0
            }
            .clamp(MIN_VIEW_DPI, MAX_VIEW_DPI);
            (Scale::Dpi(dpi), dpi)
        }
    };

    Ok((dpi, renderer.render(page, scale)?))
}

fn current_source(renderer: &Option<PageRenderer>) -> pdf_core::Result<PathBuf> {
    renderer
        .as_ref()
        .map(|r| r.path().to_path_buf())
        .ok_or_else(|| PdfError::Internal("no document is open".into()))
}

/// Return the file to read pages from, first moving reads onto a private copy
/// if `destination` is the file currently open.
///
/// The edits being written refer to the original's pages, and so does every
/// step of undo, so the original has to stay readable after it is replaced.
fn protect_source(
    renderer: &mut Option<PageRenderer>,
    copies: &mut Vec<Temporary>,
    destination: &Path,
    say: &impl Fn(Response),
) -> pdf_core::Result<PathBuf> {
    let source = current_source(renderer)?;
    if !same_file(&source, destination) {
        return Ok(source);
    }

    let copy = Temporary::scratch();
    std::fs::copy(&source, copy.path()).map_err(|error| PdfError::Io {
        path: source.clone(),
        source: error,
    })?;

    let reopened = PageRenderer::open(copy.path())?;
    let moved_to = copy.path().to_path_buf();
    *renderer = Some(reopened);
    copies.push(copy);

    tracing::info!(
        from = %source.display(),
        to = %moved_to.display(),
        "reading from a private copy of a file about to be overwritten"
    );
    say(Response::SourceMoved {
        from: source,
        to: moved_to.clone(),
    });
    Ok(moved_to)
}

/// Whether two paths name the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A document with the pending edits applied, not yet written anywhere.
struct Staged {
    document: Document,
}

impl Staged {
    /// Write to a scratch file, for the operations that read from a path.
    fn write_temporary(mut self) -> pdf_core::Result<Temporary> {
        let temporary = Temporary::scratch();
        self.document.save(temporary.path())?;
        Ok(temporary)
    }
}

/// A file removed when dropped, unless it is persisted first.
struct Temporary {
    path: PathBuf,
    keep: bool,
}

impl Temporary {
    /// A fresh path in the system's temporary directory.
    fn scratch() -> Self {
        Self {
            path: std::env::temp_dir().join(format!("foolscap-gui-{}.pdf", unique())),
            keep: false,
        }
    }

    /// A fresh, hidden path in the same directory as `destination`, so that
    /// renaming it into place is a single atomic step on one filesystem.
    fn beside(destination: &Path) -> Self {
        let name = destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document.pdf".to_string());
        Self {
            path: destination.with_file_name(format!(".{name}.foolscap-{}.tmp", unique())),
            keep: false,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Move into place over `destination`, keeping its permissions if it
    /// already exists.
    fn persist(mut self, destination: &Path) -> pdf_core::Result<()> {
        if let Ok(existing) = std::fs::metadata(destination) {
            let _ = std::fs::set_permissions(&self.path, existing.permissions());
        }

        std::fs::rename(&self.path, destination).map_err(|source| PdfError::Io {
            path: destination.to_path_buf(),
            source,
        })?;
        self.keep = true;
        Ok(())
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Apply the pending edits to a fresh copy of the document on disk.
fn stage(source: &Path, edits: &Edits) -> pdf_core::Result<Staged> {
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

/// Write a document so that `destination` is never left half-written: the
/// bytes go to a hidden file beside it, which then replaces it in one step.
fn save_atomically(document: &mut Document, destination: &Path) -> pdf_core::Result<()> {
    let scratch = Temporary::beside(destination);
    document.save(scratch.path())?;
    scratch.persist(destination)
}

fn write_edited(source: &Path, destination: &Path, edits: &Edits) -> pdf_core::Result<()> {
    let mut staged = stage(source, edits)?;
    save_atomically(&mut staged.document, destination)
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

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("foolscap-gui-test-{name}-{}", unique()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_persisted_temporary_replaces_the_destination() {
        let dir = scratch_dir("persist");
        let destination = dir.join("out.pdf");
        std::fs::write(&destination, b"old").unwrap();

        let scratch = Temporary::beside(&destination);
        assert_eq!(scratch.path().parent(), Some(dir.as_path()));
        std::fs::write(scratch.path(), b"new").unwrap();
        scratch.persist(&destination).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no stray files"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_dropped_temporary_leaves_nothing_behind() {
        let dir = scratch_dir("drop");
        let destination = dir.join("out.pdf");

        let scratch = Temporary::beside(&destination);
        std::fs::write(scratch.path(), b"partial").unwrap();
        drop(scratch);

        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_same_file_is_recognised_through_a_different_spelling() {
        let dir = scratch_dir("same");
        let file = dir.join("a.pdf");
        std::fs::write(&file, b"x").unwrap();

        assert!(same_file(&file, &dir.join(".").join("a.pdf")));
        assert!(!same_file(&file, &dir.join("missing.pdf")));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
