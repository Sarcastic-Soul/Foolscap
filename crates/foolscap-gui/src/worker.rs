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

use pdf_core::convert::{
    pdf_to_office, ImageFormat, OfficeFormat, PdfToImagesOptions, TextToPdfOptions,
};
use pdf_core::ops::FlattenOptions;
use pdf_core::render::{PageRenderer, RenderedPage, Scale};
use pdf_core::{
    CompressLevel, Document, EncryptOptions, Fit, Margins, PageRange, PageSize, PdfError, Progress,
    Rect, StampOptions,
};

/// Resolution limits for the main view, so that zooming or a very wide window
/// cannot ask MuPDF for an enormous bitmap.
const MIN_VIEW_DPI: f32 = 18.0;
const MAX_VIEW_DPI: f32 = 600.0;

/// Work the user interface asks for.
///
/// Long operations carry a `job_id` from the batch queue so progress and
/// completion can be matched to the queued row. Immediate operations
/// (`Open`, `Render`, `Save`, `Quit`) carry none.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Request {
    /// Open a document. `password` is needed only for a file that asks for
    /// one; a file unlocked earlier in the session reopens without it.
    Open {
        path: PathBuf,
        password: Option<String>,
    },
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
        job_id: u64,
    },
    Ocr {
        path: PathBuf,
        language: String,
        edits: Edits,
        job_id: u64,
    },
    /// Rasterise every page, into a folder or one zip archive.
    ExportImages {
        output: ImageOutput,
        dpi: f32,
        format: ImageFormat,
        edits: Edits,
        job_id: u64,
    },
    /// Combine several files into `output`, then open it. `unlock` is a
    /// password for one of the inputs, given after the merge asked for it.
    Merge {
        inputs: Vec<PathBuf>,
        output: PathBuf,
        unlock: Option<(PathBuf, String)>,
        job_id: u64,
    },
    /// Burst the open document into `directory`, `every` pages per piece.
    Split {
        directory: PathBuf,
        every: usize,
        edits: Edits,
        job_id: u64,
    },
    /// Draw text, an image or a signature on the chosen pages. Watermarks,
    /// page numbers and signatures all go through here; `task` says which one
    /// asked, so a failure can be worded for it.
    Stamp {
        path: PathBuf,
        options: Box<StampOptions>,
        pages: PageRange,
        task: Task,
        edits: Edits,
        job_id: u64,
    },
    /// Crop, resize or reset the page boxes.
    Crop {
        path: PathBuf,
        action: CropAction,
        pages: PageRange,
        edits: Edits,
        job_id: u64,
    },
    /// Cover a region, or every run of matching text, on the chosen pages.
    Redact {
        path: PathBuf,
        target: RedactTarget,
        pages: PageRange,
        edits: Edits,
        job_id: u64,
    },
    /// Bake forms and annotations into the page content.
    Flatten {
        path: PathBuf,
        options: FlattenOptions,
        pages: PageRange,
        edits: Edits,
        job_id: u64,
    },
    /// Write an AES-256 protected copy.
    Encrypt {
        path: PathBuf,
        user_password: String,
        owner_password: String,
        edits: Edits,
        job_id: u64,
    },
    ExportText {
        path: PathBuf,
        edits: Edits,
        job_id: u64,
    },
    ExportHtml {
        path: PathBuf,
        edits: Edits,
        job_id: u64,
    },
    FromImages {
        inputs: Vec<PathBuf>,
        output: PathBuf,
        job_id: u64,
    },
    FromOffice {
        input: PathBuf,
        output_dir: PathBuf,
        job_id: u64,
    },
    /// PDF into an office format (lossy).
    ToOffice {
        format: OfficeFormat,
        output_dir: PathBuf,
        edits: Edits,
        job_id: u64,
    },
    /// `input` is the source file; the worker reads it, so the GTK thread
    /// never does IO.
    FromText {
        input: PathBuf,
        output: PathBuf,
        job_id: u64,
    },
    FromMarkdown {
        input: PathBuf,
        output: PathBuf,
        job_id: u64,
    },
    Quit,
    /// Open a document for one side of the compare view.
    CompareOpen {
        path: PathBuf,
        password: Option<String>,
        slot: RenderSlot,
    },
    /// Render a page for comparison (left or right).
    CompareRender(CompareRenderRequest),
}

/// Where exported page images go.
#[derive(Debug, Clone)]
pub enum ImageOutput {
    /// One file per page, in this folder.
    Folder(PathBuf),
    /// Every page inside one zip archive.
    Zip(PathBuf),
}

impl ImageOutput {
    /// What the user chose, for the queue row.
    pub fn display(&self) -> String {
        match self {
            ImageOutput::Folder(path) | ImageOutput::Zip(path) => path.display().to_string(),
        }
    }
}

/// What a crop job does to the page boxes.
#[derive(Debug, Clone, Copy)]
pub enum CropAction {
    /// Show only this rectangle, in points from the bottom-left corner.
    Box(Rect),
    /// Trim this much off each edge of every page, however big the page is.
    Margins(Margins),
    /// Scale the content onto a standard page size.
    ScaleTo { size: PageSize, fit: Fit },
    /// Remove an earlier crop and show the whole page again.
    Reset,
}

/// What a redact job removes.
#[derive(Debug, Clone)]
pub enum RedactTarget {
    /// Black out this rectangle, in points from the bottom-left corner.
    Region(Rect),
    /// Remove every run of text containing this string.
    Text(String),
}

impl Request {
    /// The batch job this request belongs to, if any.
    pub fn job_id(&self) -> Option<u64> {
        match self {
            Request::Compress { job_id, .. }
            | Request::Ocr { job_id, .. }
            | Request::ExportImages { job_id, .. }
            | Request::Merge { job_id, .. }
            | Request::Split { job_id, .. }
            | Request::Stamp { job_id, .. }
            | Request::Crop { job_id, .. }
            | Request::Redact { job_id, .. }
            | Request::Flatten { job_id, .. }
            | Request::Encrypt { job_id, .. }
            | Request::ExportText { job_id, .. }
            | Request::ExportHtml { job_id, .. }
            | Request::FromImages { job_id, .. }
            | Request::FromOffice { job_id, .. }
            | Request::ToOffice { job_id, .. }
            | Request::FromText { job_id, .. }
            | Request::FromMarkdown { job_id, .. } => Some(*job_id),
            Request::Open { .. }
            | Request::Render { .. }
            | Request::Save { .. }
            | Request::Quit
            | Request::CompareOpen { .. }
            | Request::CompareRender(_) => None,
        }
    }
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
#[allow(dead_code)]
pub enum Task {
    Open,
    /// Opening a document for the compare view.
    CompareOpen,
    Render,
    Save,
    Compress,
    Ocr,
    Export,
    Merge,
    Split,
    Stamp,
    PageNumbers,
    Sign,
    Crop,
    Redact,
    Flatten,
    Encrypt,
    ExportText,
    ExportHtml,
    FromImages,
    FromOffice,
    ToOffice,
    FromText,
    FromMarkdown,
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
    /// Second document opened for comparison.
    CompareOpened {
        path: PathBuf,
        page_count: usize,
        slot: RenderSlot,
    },
    Rendered {
        page: usize,
        generation: u64,
        purpose: Purpose,
        /// The resolution used, which sets the main view's zoom level.
        dpi: f32,
        image: Arc<RenderedPage>,
    },
    /// Rendered page for comparison mode.
    CompareRendered {
        page: usize,
        generation: u64,
        purpose: Purpose,
        dpi: f32,
        image: Arc<RenderedPage>,
        slot: RenderSlot,
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
        /// The original was encrypted, and the copy written is not.
        unprotected: bool,
    },
    Finished {
        message: String,
    },
    /// Determinate progress for a queued job: `fraction` is 0.0..=1.0 when
    /// the total is known, `None` for an indeterminate step.
    JobProgress {
        job_id: u64,
        fraction: Option<f32>,
        message: String,
    },
    /// A queued job completed.
    JobFinished {
        job_id: u64,
        message: String,
    },
    Failed {
        task: Task,
        error: PdfError,
    },
}

impl Response {
    /// The batch job this response belongs to, if any.
    #[allow(dead_code)]
    pub fn job_id(&self) -> Option<u64> {
        match self {
            Response::JobProgress { job_id, .. } | Response::JobFinished { job_id, .. } => {
                Some(*job_id)
            }
            _ => None,
        }
    }
}

/// Return the compare renderer for the given slot. They are separate from the
/// main document's renderer, so the compare view never disturbs the page view.
fn renderer_for_slot<'a>(
    slot: RenderSlot,
    left: &'a mut Option<PageRenderer>,
    right: &'a mut Option<PageRenderer>,
) -> &'a mut Option<PageRenderer> {
    match slot {
        RenderSlot::Primary => left,
        RenderSlot::Secondary => right,
    }
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

/// Which side of the compare view a request or reply belongs to: `Primary` is
/// the left side, `Secondary` the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderSlot {
    Primary,
    Secondary,
}

/// Extended render request for comparison mode.
#[derive(Debug, Clone)]
pub struct CompareRenderRequest {
    pub page: usize,
    pub size: Size,
    pub purpose: Purpose,
    pub generation: u64,
    pub slot: RenderSlot,
}

fn run(requests: mpsc::Receiver<Request>, responses: async_channel::Sender<Response>) {
    let mut primary_renderer: Option<PageRenderer> = None;
    let mut compare_left: Option<PageRenderer> = None;
    let mut compare_right: Option<PageRenderer> = None;
    // Private copies of files that were overwritten while open. They live as
    // long as the window, since undo may still need their pages.
    let mut copies: Vec<Temporary> = Vec::new();
    // Passwords for the files unlocked this session, so that the open
    // document, its private copies and earlier documents that undo may return
    // to can all be read again. Held in memory only.
    let mut passwords = Passwords::default();

    let say = |response: Response| {
        // A closed channel means the window has gone; there is nothing useful
        // left to do either way.
        let _ = responses.send_blocking(response);
    };
    let fail = |task: Task, error: PdfError| say(Response::Failed { task, error });

    while let Ok(request) = requests.recv() {
        // The id of the batch job being served, if any. Progress and
        // completion are reported against it as well as on the legacy
        // string pump, so the queue panel can show a determinate fraction.
        let job_id = request.job_id();
        let pump = |progress: Progress| {
            say(Response::Progress(progress.message.clone()));
            if let Some(job_id) = job_id {
                say(Response::JobProgress {
                    job_id,
                    fraction: progress.fraction(),
                    message: progress.message,
                });
            }
        };
        let done = |message: String| {
            say(Response::Finished {
                message: message.clone(),
            });
            if let Some(job_id) = job_id {
                say(Response::JobFinished { job_id, message });
            }
        };

        match request {
            Request::Quit => break,

            Request::Open { path, password } => {
                let password = password.or_else(|| passwords.get(&path).map(str::to_string));
                match PageRenderer::open_with(&path, password.as_deref()) {
                    Ok(opened) => {
                        if let Some(password) = password {
                            passwords.insert(&path, password);
                        }
                        let page_count = opened.page_count();
                        primary_renderer = Some(opened);
                        say(Response::Opened { path, page_count });
                    }
                    Err(error) => fail(Task::Open, error),
                }
            }

            Request::CompareOpen {
                path,
                password,
                slot,
            } => {
                let password = password.or_else(|| passwords.get(&path).map(str::to_string));
                match PageRenderer::open_with(&path, password.as_deref()) {
                    Ok(opened) => {
                        if let Some(password) = password {
                            passwords.insert(&path, password);
                        }
                        let page_count = opened.page_count();
                        *renderer_for_slot(slot, &mut compare_left, &mut compare_right) =
                            Some(opened);
                        say(Response::CompareOpened {
                            path,
                            page_count,
                            slot,
                        });
                    }
                    Err(error) => fail(Task::CompareOpen, error),
                }
            }

            Request::Render {
                page,
                size,
                purpose,
                generation,
            } => {
                let Some(renderer) = primary_renderer.as_ref() else {
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

            Request::CompareRender(req) => {
                let renderer_slot =
                    renderer_for_slot(req.slot, &mut compare_left, &mut compare_right);
                let Some(renderer) = renderer_slot.as_ref() else {
                    continue;
                };

                match render(renderer, req.page, req.size) {
                    Ok((dpi, image)) => say(Response::CompareRendered {
                        page: req.page,
                        generation: req.generation,
                        purpose: req.purpose,
                        dpi,
                        image,
                        slot: req.slot,
                    }),
                    Err(error) => fail(Task::Render, error),
                }
            }

            Request::Save { path, edits } => {
                let result = protect_source(
                    &mut primary_renderer,
                    &mut copies,
                    &mut passwords,
                    &path,
                    &say,
                )
                .and_then(|source| write_edited(&source, &passwords, &path, &edits));

                match result {
                    Ok(unprotected) => say(Response::Saved { path, unprotected }),
                    Err(error) => fail(Task::Save, error),
                }
            }

            Request::Compress {
                path,
                level,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(
                        &mut primary_renderer,
                        &mut copies,
                        &mut passwords,
                        &path,
                        &say,
                    )?;
                    let mut doc = stage(&source, &passwords, &edits)?.document;
                    let unprotected = doc.was_encrypted();
                    let report = pdf_core::compress(&mut doc, level)?;
                    save_atomically(&mut doc, &path)?;
                    Ok(format!(
                        "Compressed to {} ({:.0}% smaller){}",
                        path.display(),
                        report.ratio_saved() * 100.0,
                        unprotected_note(unprotected)
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::Compress, error),
                }
            }

            Request::Ocr {
                path,
                language,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(
                        &mut primary_renderer,
                        &mut copies,
                        &mut passwords,
                        &path,
                        &say,
                    )?;

                    // Recognition works from a file, so the edited document has
                    // to exist on disk first. It is written decrypted, so no
                    // password is needed past this point.
                    let staged = stage(&source, &passwords, &edits)?;
                    let unprotected = staged.document.was_encrypted();
                    let intermediate = staged.write_temporary()?;

                    let options = pdf_core::OcrOptions {
                        language,
                        ..Default::default()
                    };

                    let mut pump = pump;
                    let report = pdf_core::ocr_with_progress(
                        intermediate.path(),
                        &path,
                        &options,
                        Some(&mut pump),
                    )?;

                    let mut left_alone = String::new();
                    if report.pages_already_text > 0 {
                        left_alone
                            .push_str(&format!("; {} already had text", report.pages_already_text));
                    }
                    if report.pages_without_text > 0 {
                        left_alone.push_str(&format!(
                            "; {} had nothing to read",
                            report.pages_without_text
                        ));
                    }
                    Ok(format!(
                        "Recognised {} of {} page(s){} into {}{}",
                        report.pages_recognised,
                        report.pages_total,
                        left_alone,
                        path.display(),
                        unprotected_note(unprotected)
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::Ocr, error),
                }
            }

            Request::ExportImages {
                output,
                dpi,
                format,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = current_source(&primary_renderer)?;
                    let intermediate =
                        stage(&source, &passwords, &edits)?.write_temporary_as(&source)?;

                    let options = PdfToImagesOptions {
                        dpi,
                        format,
                        ..Default::default()
                    };

                    let mut pump = pump;
                    match &output {
                        ImageOutput::Folder(directory) => {
                            let written = pdf_core::convert::pdf_to_images_with_progress(
                                intermediate.path(),
                                directory,
                                &options,
                                Some(&mut pump),
                            )?;
                            Ok(format!(
                                "Exported {} image(s) to {}",
                                written.len(),
                                directory.display()
                            ))
                        }
                        ImageOutput::Zip(archive) => {
                            let names = pdf_core::convert::pdf_to_zip_with_progress(
                                intermediate.path(),
                                archive,
                                &options,
                                Some(&mut pump),
                            )?;
                            Ok(format!(
                                "Packed {} image(s) into {}",
                                names.len(),
                                archive.display()
                            ))
                        }
                    }
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::Export, error),
                }
            }

            Request::Merge {
                inputs,
                output,
                unlock,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<PageRenderer> {
                    // Merging over the open file is allowed; the private copy
                    // keeps the way back to it for undo.
                    if primary_renderer.is_some() {
                        protect_source(
                            &mut primary_renderer,
                            &mut copies,
                            &mut passwords,
                            &output,
                            &say,
                        )?;
                    }

                    // Each protected input may have its own password and the
                    // merge takes only one, so unlocked inputs are first
                    // written out decrypted beside the output, which will hold
                    // the same pages unencrypted anyway.
                    let mut decrypted = Vec::new();
                    let mut sources = Vec::with_capacity(inputs.len());
                    for input in &inputs {
                        let password = match &unlock {
                            Some((path, password)) if path == input => Some(password.clone()),
                            _ => passwords.get(input).map(str::to_string),
                        };
                        let Some(password) = password else {
                            sources.push(input.clone());
                            continue;
                        };

                        let mut doc = Document::open_with(input, Some(&password))?;
                        passwords.insert(input, password);
                        let copy = Temporary::beside(&output);
                        doc.save(copy.path())?;
                        sources.push(copy.path().to_path_buf());
                        decrypted.push(copy);
                    }

                    let scratch = Temporary::beside(&output);
                    let mut pump = pump;
                    pdf_core::merge_with_progress(&sources, scratch.path(), None, Some(&mut pump))?;
                    drop(decrypted);
                    scratch.persist(&output)?;

                    PageRenderer::open(&output)
                })();

                match result {
                    Ok(opened) => {
                        let page_count = opened.page_count();
                        primary_renderer = Some(opened);
                        say(Response::Opened {
                            path: output.clone(),
                            page_count,
                        });
                        done(format!("Merged into {}", output.display()));
                    }
                    Err(error) => fail(Task::Merge, error),
                }
            }

            Request::Split {
                directory,
                every,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = current_source(&primary_renderer)?;
                    let intermediate =
                        stage(&source, &passwords, &edits)?.write_temporary_as(&source)?;
                    let every = every.max(1);
                    let spec = pdf_core::SplitSpec::Every(every);
                    let mut pump = pump;
                    let written = pdf_core::split_with_progress(
                        intermediate.path(),
                        &spec,
                        &directory,
                        None,
                        Some(&mut pump),
                        &pdf_core::SplitOptions::default(),
                    )?;
                    Ok(format!(
                        "Split into {} file(s) in {}",
                        written.len(),
                        directory.display()
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::Split, error),
                }
            }

            Request::FromImages {
                inputs,
                output,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    if primary_renderer.is_some() {
                        protect_source(
                            &mut primary_renderer,
                            &mut copies,
                            &mut passwords,
                            &output,
                            &say,
                        )?;
                    }
                    let options = pdf_core::convert::ImagesToPdfOptions::default();
                    let mut pump = pump;
                    pdf_core::convert::images_to_pdf_with_progress(
                        &inputs,
                        &output,
                        options,
                        Some(&mut pump),
                    )?;
                    Ok(format!(
                        "Made {} from {} image(s)",
                        output.display(),
                        inputs.len()
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::FromImages, error),
                }
            }

            Request::FromOffice {
                input,
                output_dir,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let written = pdf_core::convert::office::office_to_pdf(&input, &output_dir)?;
                    Ok(format!("Converted to {}", written.display()))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::FromOffice, error),
                }
            }

            Request::Stamp {
                path,
                options,
                pages,
                task,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(
                        &mut primary_renderer,
                        &mut copies,
                        &mut passwords,
                        &path,
                        &say,
                    )?;
                    let mut staged = stage(&source, &passwords, &edits)?;
                    let unprotected = staged.document.was_encrypted();

                    let mut pump = pump;
                    pdf_core::stamp_with_progress(
                        &mut staged.document,
                        &pages,
                        &options,
                        Some(&mut pump),
                    )?;

                    save_atomically(&mut staged.document, &path)?;
                    let verb = match task {
                        Task::PageNumbers => "Numbered the pages of",
                        Task::Sign => "Signed",
                        _ => "Watermarked",
                    };
                    Ok(format!(
                        "{verb} {}{}",
                        path.display(),
                        unprotected_note(unprotected)
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(task, error),
                }
            }

            Request::Crop {
                path,
                action,
                pages,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(
                        &mut primary_renderer,
                        &mut copies,
                        &mut passwords,
                        &path,
                        &say,
                    )?;
                    let mut staged = stage(&source, &passwords, &edits)?;
                    let unprotected = staged.document.was_encrypted();

                    let mut pump = pump;
                    let doc = &mut staged.document;
                    let verb = match action {
                        CropAction::Box(rect) => {
                            pdf_core::crop_with_progress(doc, &pages, rect, Some(&mut pump))?;
                            "Cropped"
                        }
                        CropAction::Margins(margins) => {
                            pdf_core::crop_margins_with_progress(
                                doc,
                                &pages,
                                margins,
                                Some(&mut pump),
                            )?;
                            "Cropped"
                        }
                        CropAction::ScaleTo { size, fit } => {
                            pdf_core::scale_to_with_progress(
                                doc,
                                &pages,
                                size,
                                fit,
                                Some(&mut pump),
                            )?;
                            "Resized"
                        }
                        CropAction::Reset => {
                            pdf_core::reset_crop(doc, &pages)?;
                            "Restored the full page of"
                        }
                    };

                    save_atomically(&mut staged.document, &path)?;
                    Ok(format!(
                        "{verb} {}{}",
                        path.display(),
                        unprotected_note(unprotected)
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::Crop, error),
                }
            }

            Request::Redact {
                path,
                target,
                pages,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(
                        &mut primary_renderer,
                        &mut copies,
                        &mut passwords,
                        &path,
                        &say,
                    )?;
                    let mut staged = stage(&source, &passwords, &edits)?;
                    let unprotected = staged.document.was_encrypted();

                    let mut pump = pump;
                    let report = match &target {
                        RedactTarget::Region(rect) => pdf_core::redact_with_progress(
                            &mut staged.document,
                            &pages,
                            *rect,
                            Some(&mut pump),
                        )?,
                        RedactTarget::Text(needle) => pdf_core::redact_find_with_progress(
                            &mut staged.document,
                            &pages,
                            needle,
                            Some(&mut pump),
                        )?,
                    };

                    save_atomically(&mut staged.document, &path)?;
                    Ok(format!(
                        "Redacted {} page(s), removing {} run(s) of text, into {}{}",
                        report.pages_redacted,
                        report.runs_removed,
                        path.display(),
                        unprotected_note(unprotected)
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::Redact, error),
                }
            }

            Request::Flatten {
                path,
                options,
                pages,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(
                        &mut primary_renderer,
                        &mut copies,
                        &mut passwords,
                        &path,
                        &say,
                    )?;
                    let mut staged = stage(&source, &passwords, &edits)?;
                    let unprotected = staged.document.was_encrypted();

                    let mut pump = pump;
                    let report = pdf_core::ops::flatten_with_progress(
                        &mut staged.document,
                        &pages,
                        options,
                        Some(&mut pump),
                    )?;

                    save_atomically(&mut staged.document, &path)?;
                    Ok(format!(
                        "Flattened {} form field(s) and {} annotation(s) into {}{}",
                        report.widgets_flattened,
                        report.annots_flattened,
                        path.display(),
                        unprotected_note(unprotected)
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::Flatten, error),
                }
            }

            Request::Encrypt {
                path,
                user_password,
                owner_password,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = protect_source(
                        &mut primary_renderer,
                        &mut copies,
                        &mut passwords,
                        &path,
                        &say,
                    )?;
                    let mut staged = stage(&source, &passwords, &edits)?;

                    let options = EncryptOptions {
                        user_password,
                        owner_password,
                        ..EncryptOptions::default()
                    };
                    let scratch = Temporary::beside(&path);
                    staged.document.save_encrypted(scratch.path(), &options)?;
                    scratch.persist(&path)?;

                    Ok(format!("Wrote the protected copy {}", path.display()))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::Encrypt, error),
                }
            }

            Request::ExportText {
                path,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = current_source(&primary_renderer)?;
                    let intermediate =
                        stage(&source, &passwords, &edits)?.write_temporary_as(&source)?;

                    // Text comes out of MuPDF, which reads a file; the staged
                    // copy has the edits applied and no password.
                    let reader = PageRenderer::open(intermediate.path())?;
                    let total = reader.page_count();
                    let mut text = String::new();
                    for page in 0..total {
                        pump(Progress::new(
                            page,
                            Some(total),
                            format!("reading page {} of {total}", page + 1),
                        ));
                        if page > 0 {
                            text.push_str("\n\n");
                        }
                        text.push_str(reader.page_text(page)?.trim_end());
                    }
                    text.push('\n');

                    let scratch = Temporary::beside(&path);
                    std::fs::write(scratch.path(), text).map_err(|source| PdfError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    scratch.persist(&path)?;
                    Ok(format!(
                        "Extracted the text of {total} page(s) to {}",
                        path.display()
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::ExportText, error),
                }
            }

            Request::ExportHtml {
                path,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = current_source(&primary_renderer)?;
                    let intermediate = stage(&source, &passwords, &edits)?.write_temporary()?;

                    let mut pump = pump;
                    pdf_core::convert::pdf_to_html_with_progress(
                        intermediate.path(),
                        &path,
                        &pdf_core::convert::PdfToHtmlOptions::default(),
                        Some(&mut pump),
                    )?;
                    Ok(format!("Wrote {}", path.display()))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::ExportHtml, error),
                }
            }

            Request::ToOffice {
                format,
                output_dir,
                edits,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    let source = current_source(&primary_renderer)?;
                    let intermediate = stage(&source, &passwords, &edits)?.write_temporary()?;

                    // The staged copy is never encrypted, so it needs no
                    // password, and LibreOffice gives no progress to report.
                    let produced = pdf_to_office(intermediate.path(), format, &output_dir, None)?;
                    Ok(format!("Converted to {}", produced.display()))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::ToOffice, error),
                }
            }

            Request::FromText {
                input,
                output,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    if primary_renderer.is_some() {
                        protect_source(
                            &mut primary_renderer,
                            &mut copies,
                            &mut passwords,
                            &output,
                            &say,
                        )?;
                    }
                    let mut pump = pump;
                    pdf_core::convert::text_file_to_pdf_with_progress(
                        &input,
                        &output,
                        &TextToPdfOptions::default(),
                        Some(&mut pump),
                    )?;
                    Ok(format!(
                        "Made {} from {}",
                        output.display(),
                        file_label(&input)
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::FromText, error),
                }
            }

            Request::FromMarkdown {
                input,
                output,
                job_id: _,
            } => {
                let result = (|| -> pdf_core::Result<String> {
                    if primary_renderer.is_some() {
                        protect_source(
                            &mut primary_renderer,
                            &mut copies,
                            &mut passwords,
                            &output,
                            &say,
                        )?;
                    }
                    let mut pump = pump;
                    pdf_core::convert::markdown_file_to_pdf_with_progress(
                        &input,
                        &output,
                        &TextToPdfOptions::default(),
                        Some(&mut pump),
                    )?;
                    Ok(format!(
                        "Made {} from {}",
                        output.display(),
                        file_label(&input)
                    ))
                })();

                match result {
                    Ok(message) => done(message),
                    Err(error) => fail(Task::FromMarkdown, error),
                }
            }
        }
    }
}

/// The last part of a path, for a status message.
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
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
    passwords: &mut Passwords,
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

    // A byte-for-byte copy, so it needs the same password as the original.
    let password = passwords.get(&source).map(str::to_string);
    let reopened = PageRenderer::open_with(copy.path(), password.as_deref())?;
    let moved_to = copy.path().to_path_buf();
    if let Some(password) = password {
        passwords.insert(&moved_to, password);
    }
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

    /// Like `write_temporary`, but the file keeps `source`'s name, for the
    /// operations that name their output files after their input.
    fn write_temporary_as(mut self, source: &Path) -> pdf_core::Result<Temporary> {
        let temporary = Temporary::scratch_named(source)?;
        self.document.save(temporary.path())?;
        Ok(temporary)
    }
}

/// A file removed when dropped, unless it is persisted first.
struct Temporary {
    path: PathBuf,
    keep: bool,
    /// A private directory made for this file, removed with it.
    directory: Option<PathBuf>,
}

impl Temporary {
    /// A fresh path in the system's temporary directory.
    fn scratch() -> Self {
        Self {
            path: std::env::temp_dir().join(format!("foolscap-gui-{}.pdf", unique())),
            keep: false,
            directory: None,
        }
    }

    /// A fresh path inside its own temporary directory, with the file name of
    /// `source`.
    fn scratch_named(source: &Path) -> pdf_core::Result<Self> {
        let directory = std::env::temp_dir().join(format!("foolscap-gui-{}", unique()));
        std::fs::create_dir_all(&directory).map_err(|error| PdfError::Io {
            path: directory.clone(),
            source: error,
        })?;
        let name = source
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_else(|| "document.pdf".into());
        Ok(Self {
            path: directory.join(name),
            keep: false,
            directory: Some(directory),
        })
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
            directory: None,
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
        if let Some(directory) = &self.directory {
            let _ = std::fs::remove_dir(directory);
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
fn stage(source: &Path, passwords: &Passwords, edits: &Edits) -> pdf_core::Result<Staged> {
    let mut doc = Document::open_with(source, passwords.get(source))?;

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

/// Write the edited document, returning whether the original was encrypted;
/// the copy written never is.
fn write_edited(
    source: &Path,
    passwords: &Passwords,
    destination: &Path,
    edits: &Edits,
) -> pdf_core::Result<bool> {
    let mut staged = stage(source, passwords, edits)?;
    let unprotected = staged.document.was_encrypted();
    save_atomically(&mut staged.document, destination)?;
    Ok(unprotected)
}

/// Text added to a status message when the file written dropped the
/// original's encryption.
fn unprotected_note(unprotected: bool) -> &'static str {
    if unprotected {
        ". Saved without password protection"
    } else {
        ""
    }
}

/// Passwords by the file they unlock.
#[derive(Default)]
struct Passwords(HashMap<PathBuf, String>);

impl Passwords {
    fn get(&self, path: &Path) -> Option<&str> {
        self.0.get(path).map(String::as_str)
    }

    fn insert(&mut self, path: &Path, password: String) {
        self.0.insert(path.to_path_buf(), password);
    }
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

    /// Write a one-page PDF with Ghostscript, protected by `password` when
    /// one is given. Returns false when Ghostscript is not installed.
    fn ghostscript_pdf(output: &Path, password: Option<&str>) -> bool {
        let mut command = std::process::Command::new("gs");
        command.args(["-q", "-dBATCH", "-dNOPAUSE", "-sDEVICE=pdfwrite"]);
        if let Some(password) = password {
            command.args([
                format!("-sUserPassword={password}"),
                "-sOwnerPassword=owner".to_string(),
                "-dEncryptionR=3".to_string(),
                "-dKeyLength=128".to_string(),
            ]);
        }
        command
            .arg(format!("-sOutputFile={}", output.display()))
            .args(["-c", "showpage"]);
        match command.status() {
            Ok(status) => {
                assert!(status.success(), "gs failed");
                true
            }
            Err(_) => false,
        }
    }

    /// The next response that is not a progress report.
    fn answer(from_worker: &async_channel::Receiver<Response>) -> Response {
        loop {
            match from_worker.recv_blocking().expect("worker stopped") {
                Response::Progress(_)
                | Response::JobProgress { .. }
                | Response::JobFinished { .. } => continue,
                other => return other,
            }
        }
    }

    #[test]
    fn a_protected_document_opens_with_its_password_and_saves_unprotected() {
        let dir = scratch_dir("unlock");
        let locked = dir.join("locked.pdf");
        if !ghostscript_pdf(&locked, Some("secret")) {
            eprintln!("skipping: Ghostscript (gs) is not installed");
            return;
        }

        let (to_worker, from_worker) = spawn();
        let open = |password: Option<&str>| Request::Open {
            path: locked.clone(),
            password: password.map(str::to_string),
        };

        to_worker.send(open(None)).unwrap();
        assert!(matches!(
            answer(&from_worker),
            Response::Failed { task: Task::Open, error: PdfError::Encrypted(path) } if path == locked
        ));

        to_worker.send(open(Some("wrong"))).unwrap();
        assert!(matches!(
            answer(&from_worker),
            Response::Failed {
                task: Task::Open,
                error: PdfError::WrongPassword(_)
            }
        ));

        to_worker.send(open(Some("secret"))).unwrap();
        assert!(matches!(
            answer(&from_worker),
            Response::Opened { page_count: 1, .. }
        ));

        // Saving over the open file moves reads to a private copy, which
        // needs the same password, then writes an unencrypted file.
        let mut rotations = HashMap::new();
        rotations.insert(0, 90);
        to_worker
            .send(Request::Save {
                path: locked.clone(),
                edits: Edits {
                    order: vec![0],
                    rotations,
                },
            })
            .unwrap();
        assert!(matches!(answer(&from_worker), Response::SourceMoved { .. }));
        assert!(matches!(
            answer(&from_worker),
            Response::Saved {
                unprotected: true,
                ..
            }
        ));
        assert_eq!(Document::open(&locked).unwrap().page_count(), 1);

        // Pages still draw from the protected private copy.
        to_worker
            .send(Request::Render {
                page: 0,
                size: Size::Box(64),
                purpose: Purpose::Thumbnail,
                generation: 0,
            })
            .unwrap();
        assert!(matches!(answer(&from_worker), Response::Rendered { .. }));

        to_worker.send(Request::Quit).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_merge_asks_for_each_protected_input() {
        let dir = scratch_dir("merge-unlock");
        let plain = dir.join("plain.pdf");
        let first = dir.join("first.pdf");
        let second = dir.join("second.pdf");
        if !ghostscript_pdf(&plain, None) {
            eprintln!("skipping: Ghostscript (gs) is not installed");
            return;
        }
        ghostscript_pdf(&first, Some("one"));
        ghostscript_pdf(&second, Some("two"));

        let output = dir.join("merged.pdf");
        let (to_worker, from_worker) = spawn();
        let merge = |unlock: Option<(&PathBuf, &str)>| Request::Merge {
            inputs: vec![plain.clone(), first.clone(), second.clone()],
            output: output.clone(),
            unlock: unlock.map(|(path, password)| (path.clone(), password.to_string())),
            job_id: 0,
        };

        to_worker.send(merge(None)).unwrap();
        assert!(matches!(
            answer(&from_worker),
            Response::Failed { task: Task::Merge, error: PdfError::Encrypted(path) } if path == first
        ));

        to_worker.send(merge(Some((&first, "two")))).unwrap();
        assert!(matches!(
            answer(&from_worker),
            Response::Failed { task: Task::Merge, error: PdfError::WrongPassword(path) } if path == first
        ));

        // The first file's password is remembered once it has worked, so the
        // next attempt stops at the second.
        to_worker.send(merge(Some((&first, "one")))).unwrap();
        assert!(matches!(
            answer(&from_worker),
            Response::Failed { task: Task::Merge, error: PdfError::Encrypted(path) } if path == second
        ));

        to_worker.send(merge(Some((&second, "two")))).unwrap();
        assert!(matches!(
            answer(&from_worker),
            Response::Opened { page_count: 3, .. }
        ));
        assert_eq!(Document::open(&output).unwrap().page_count(), 3);

        to_worker.send(Request::Quit).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "stray files: {leftovers:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
