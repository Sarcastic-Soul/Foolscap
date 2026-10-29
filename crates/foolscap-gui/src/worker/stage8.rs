//! The worker side of the later tools: operations that change a document,
//! make a new one, read one for a report, or export it.
//!
//! Each function here is plain `pdf-core` work with no GTK types, so the
//! worker loop only has to pick the right one and report the result.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pdf_core::ops::bookmark::Bookmark;
use pdf_core::ops::nup::{booklet_with_progress, nup_with_progress, BookletOptions, NupOptions};
use pdf_core::ops::pdfa::{check_pdfa, to_pdfa, PdfALevel, PdfAReport};
use pdf_core::ops::svg::{plan as plan_svg, to_svg_with_progress, SvgOptions};
use pdf_core::ops::{
    add_attachment, add_bookmark, fill_form, halve, linearize_with_progress, parse_form_data,
    set_layer_visibility, set_page_labels, FieldValue, HalveOptions, LabelRange,
};
use pdf_core::{
    create_blank, qr_code, to_markdown, verify_signatures, Document, MarkdownOptions, PageRange,
    PageSize, PdfError, ProgressFn, QrOptions, Result,
};

use super::Task;

/// A change to the document being edited.
#[derive(Debug, Clone)]
pub enum Transform {
    /// Reorder objects so the first page loads without the rest.
    Linearize,
    /// Convert to an archival PDF/A level.
    ToPdfa(PdfALevel),
    /// Several pages per sheet.
    NUp(NupOptions),
    /// Pages ordered for folding into a booklet.
    Booklet(BookletOptions),
    /// Cut every page in two.
    Halve(HalveOptions),
    /// Add an outline entry under `parent` (0 for the top level).
    Bookmark {
        title: String,
        page: usize,
        parent: usize,
    },
    /// Fill form fields from a JSON file of `"name": value` pairs.
    FillForm(PathBuf),
    /// Embed a file, named after its own file name.
    Attach(PathBuf),
    /// Show or hide an optional content layer by name.
    SetLayer { name: String, visible: bool },
    /// Replace the page labels; an empty list clears them.
    PageLabels(Vec<LabelRange>),
}

impl Transform {
    /// The task whose heading names a failure of this transform.
    pub fn task(&self) -> Task {
        match self {
            Transform::Linearize | Transform::ToPdfa(_) => Task::Archive,
            Transform::NUp(_) | Transform::Booklet(_) | Transform::Halve(_) => Task::Layout,
            Transform::Bookmark { .. } => Task::Bookmark,
            Transform::FillForm(_) => Task::FillForm,
            Transform::Attach(_) => Task::Attach,
            Transform::SetLayer { .. } => Task::Layers,
            Transform::PageLabels(_) => Task::PageLabels,
        }
    }
}

/// Apply `transform`, returning the new document and a one-line summary.
pub fn apply(
    document: Document,
    transform: &Transform,
    progress: ProgressFn<'_>,
) -> Result<(Document, String)> {
    match transform {
        Transform::Linearize => {
            let mut document = document;
            linearize_with_progress(&mut document, progress)?;
            Ok((document, "Optimized for web viewing".into()))
        }
        Transform::ToPdfa(level) => {
            let mut document = document;
            let report = to_pdfa(&mut document, *level)?;
            let verdict = if report.conformant {
                "passes the structural checks"
            } else {
                "has structural issues; run Check PDF/A for the list"
            };
            Ok((
                document,
                format!("Converted to {}, which {verdict}", label(*level)),
            ))
        }
        Transform::NUp(options) => {
            let before = document.page_count();
            let imposed = nup_with_progress(&document, options.clone(), Some(progress))?;
            let after = imposed.page_count();
            Ok((
                imposed,
                format!("{before} pages laid out as {after} sheets"),
            ))
        }
        Transform::Booklet(options) => {
            let before = document.page_count();
            let imposed = booklet_with_progress(&document, options.clone(), Some(progress))?;
            let after = imposed.page_count();
            Ok((
                imposed,
                format!("{before} pages imposed onto {after} sheets"),
            ))
        }
        Transform::Halve(options) => {
            let mut document = document;
            let before = document.page_count();
            halve(&mut document, options)?;
            let after = document.page_count();
            Ok((document, format!("{before} pages cut into {after}")))
        }
        Transform::Bookmark {
            title,
            page,
            parent,
        } => {
            let mut document = document;
            add_bookmark(&mut document, Bookmark::new(title.clone(), *page), *parent)?;
            Ok((document, format!("Added the bookmark “{title}”")))
        }
        Transform::FillForm(data_path) => {
            let mut document = document;
            let raw = std::fs::read_to_string(data_path).map_err(|source| PdfError::Io {
                path: data_path.clone(),
                source,
            })?;
            let data: BTreeMap<String, FieldValue> = parse_form_data(&raw)?;
            let report = fill_form(&mut document, &data)?;

            let mut summary = format!("{} field(s) filled", report.filled.len());
            if !report.not_found.is_empty() {
                summary.push_str(&format!("; not found: {}", report.not_found.join(", ")));
            }
            if !report.unsupported.is_empty() {
                summary.push_str(&format!("; unsupported: {}", report.unsupported.join(", ")));
            }
            Ok((document, summary))
        }
        Transform::Attach(file) => {
            let mut document = document;
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .ok_or_else(|| {
                    PdfError::InvalidAttachment(format!("{} has no file name", file.display()))
                })?;
            let data = std::fs::read(file).map_err(|source| PdfError::Io {
                path: file.clone(),
                source,
            })?;
            add_attachment(&mut document, &name, &data)?;
            Ok((document, format!("Embedded {name}")))
        }
        Transform::SetLayer { name, visible } => {
            let mut document = document;
            set_layer_visibility(&mut document, name, *visible)?;
            let verb = if *visible { "Showed" } else { "Hid" };
            Ok((document, format!("{verb} the layer “{name}”")))
        }
        Transform::PageLabels(ranges) => {
            let mut document = document;
            set_page_labels(&mut document, ranges)?;
            let summary = if ranges.is_empty() {
                "Cleared the page labels"
            } else {
                "Set the page labels"
            };
            Ok((document, summary.into()))
        }
    }
}

/// The name of an archival level, as the dialog shows it.
pub fn label(level: PdfALevel) -> &'static str {
    match level {
        PdfALevel::A1b => "PDF/A-1b",
        PdfALevel::A2b => "PDF/A-2b",
        PdfALevel::A3b => "PDF/A-3b",
    }
}

/// A document made from nothing.
#[derive(Debug, Clone)]
pub enum Creation {
    /// Blank pages of one size.
    Blank { pages: usize, size: PageSize },
    /// A single page carrying a QR code.
    Qr { text: String, options: QrOptions },
}

impl Creation {
    pub fn task(&self) -> Task {
        match self {
            Creation::Blank { .. } => Task::Create,
            Creation::Qr { .. } => Task::QrCode,
        }
    }
}

pub fn create(creation: &Creation) -> Result<Document> {
    match creation {
        Creation::Blank { pages, size } => create_blank(*pages, *size),
        Creation::Qr { text, options } => qr_code(text, options),
    }
}

/// A read-only check whose result goes into a report dialog.
#[derive(Debug, Clone, Copy)]
pub enum Inspection {
    /// Every digital signature in the file.
    Signatures,
    /// PDF/A conformance, using the structural rules the core checks.
    PdfA,
}

/// What an inspection found, ready for the dialog and the status bar.
pub struct Inspected {
    pub title: String,
    pub body: String,
    pub summary: String,
}

/// Inspect the file at `source`, opened with `password` when it needs one.
pub fn inspect(source: &Path, password: Option<&str>, which: Inspection) -> Result<Inspected> {
    let document = Document::open_with(source, password)?;
    match which {
        Inspection::Signatures => signatures_report(&document),
        Inspection::PdfA => pdfa_report(&document),
    }
}

fn signatures_report(document: &Document) -> Result<Inspected> {
    let reports = verify_signatures(document)?;
    if reports.is_empty() {
        return Ok(Inspected {
            title: "Signatures".into(),
            body: "This document has no digital signatures.".into(),
            summary: "No signatures found".into(),
        });
    }

    let mut lines = Vec::new();
    for report in &reports {
        let trust = match &report.trust {
            Some(pdf_core::Trust::Trusted) => "trusted: the chain reaches a system root".into(),
            Some(pdf_core::Trust::Untrusted(why)) => format!("NOT trusted: {why}"),
            None => "not checked".to_string(),
        };
        lines.push(format!(
            "{}\n  signer:    {}\n  integrity: {}\n  trust:     {}\n  detail:    {}",
            report.name.as_deref().unwrap_or("(unnamed)"),
            report.signer.as_deref().unwrap_or("unknown"),
            if report.intact { "intact" } else { "BROKEN" },
            trust,
            report.reason,
        ));
    }

    let broken = reports.iter().filter(|report| !report.intact).count();
    Ok(Inspected {
        title: "Signatures".into(),
        body: lines.join("\n\n"),
        summary: format!("{} signature(s) checked, {broken} broken", reports.len()),
    })
}

fn pdfa_report(document: &Document) -> Result<Inspected> {
    let report: PdfAReport = check_pdfa(document)?;
    let mut body = if report.conformant {
        "No structural problems were found.\n\nThis checks the rules Foolscap enforces \
         itself. It is not a veraPDF validation."
            .to_string()
    } else {
        let mut lines = vec!["Structural problems found:".to_string()];
        lines.extend(report.issues.iter().map(|issue| format!("  • {issue}")));
        lines.join("\n")
    };
    if !report.unembedded_fonts.is_empty() {
        body.push_str(&format!(
            "\n\nFonts not embedded: {}",
            report.unembedded_fonts.join(", ")
        ));
    }

    Ok(Inspected {
        title: "PDF/A check".into(),
        body,
        summary: if report.conformant {
            "PDF/A check passed".into()
        } else {
            format!("PDF/A check found {} issue(s)", report.issues.len())
        },
    })
}

/// Write `document` as Markdown into `output`.
pub fn export_markdown(document: &Document, output: &Path) -> Result<String> {
    let text = to_markdown(document, &MarkdownOptions::default())?;
    std::fs::write(output, text).map_err(|source| PdfError::Io {
        path: output.to_path_buf(),
        source,
    })?;
    Ok(format!("Wrote {}", output.display()))
}

/// Write one SVG per selected page of the saved file at `source` into
/// `out_dir`. Refuses to overwrite any file the export would produce.
pub fn export_svg(
    source: &Path,
    password: Option<&str>,
    out_dir: &Path,
    pages: PageRange,
    progress: ProgressFn<'_>,
) -> Result<String> {
    let options = SvgOptions {
        pages,
        password: password.map(str::to_string),
    };

    for path in plan_svg(source, out_dir, &options)? {
        if path.exists() {
            return Err(PdfError::OutputExists(path));
        }
    }

    let written = to_svg_with_progress(source, out_dir, &options, Some(progress))?;
    Ok(format!(
        "Wrote {} SVG file(s) to {}",
        written.len(),
        out_dir.display()
    ))
}
