//! The window side of the later tools: each one's dialog, and the request it
//! sends to the worker. Work happens in `worker::stage8`; this file only
//! collects choices and hands them over.

use super::*;

use pdf_core::ops::nup::{BookletOptions, NupOptions, PageOrder};
use pdf_core::ops::pdfa::PdfALevel;
use pdf_core::ops::HalveOptions;
use pdf_core::{Binding, Fit, HalveDirection, Margins, PageSize, QrEcc, QrOptions};

use crate::worker::stage8::{Creation, Inspection, Transform};

/// Paper choices for the layout tools, as the core names them.
const PAPERS: &[(&str, &str)] = &[("A4", "a4"), ("Letter", "letter"), ("A3", "a3")];

fn paper_labels() -> Vec<&'static str> {
    PAPERS.iter().map(|(label, _)| *label).collect()
}

fn paper_at(index: u32) -> &'static str {
    PAPERS
        .get(index as usize)
        .map(|(_, spec)| *spec)
        .unwrap_or("a4")
}

/// A number typed into an entry, with a message naming what it was for.
fn number<T: std::str::FromStr>(entry: &gtk4::Entry, what: &str) -> Result<T, String> {
    entry
        .text()
        .trim()
        .parse()
        .map_err(|_| format!("Enter {what} as a number."))
}

/// A dialog that shows a multi-line result and is closed with one button.
pub(super) fn show_report(parent: &ApplicationWindow, title: &str, body: &str) {
    let dialog = gtk4::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .default_width(560)
        .default_height(420)
        .build();

    let text = Label::builder()
        .label(body)
        .xalign(0.0)
        .yalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();

    let scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .build();
    text.set_margin_top(12);
    text.set_margin_bottom(12);
    text.set_margin_start(12);
    text.set_margin_end(12);
    scroller.set_child(Some(&text));

    let close = gtk4::Button::with_label("Close");
    close.set_halign(gtk4::Align::End);
    close.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| dialog.close()
    ));

    let content = GtkBox::new(Orientation::Vertical, 12);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.append(&scroller);
    content.append(&close);

    dialog.set_child(Some(&content));
    dialog.present();
}

impl App {
    /// Rewrite the file with its objects in web order.
    pub(super) fn linearize(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        self.write_pdf_job("-web", "Optimize for web", |path, edits, job_id| {
            Request::Transform {
                path,
                transform: Transform::Linearize,
                edits,
                job_id,
            }
        });
    }

    /// Write an archival copy at the chosen PDF/A level.
    pub(super) fn pdf_a(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        show_form(
            &self.ui.window,
            "Archival PDF/A",
            "Choose where to save…",
            |fields| {
                let level = dropdown(&[
                    "PDF/A-1b, the most widely readable",
                    "PDF/A-2b, transparency allowed",
                    "PDF/A-3b, attachments allowed",
                ]);
                fields.add("Level", &level);
                fields.note(
                    "Foolscap checks the rules it enforces itself. That is not a veraPDF \
                     validation, so check the result with a validator before relying on it.",
                );
                Box::new(move || {
                    Ok(match level.selected() {
                        0 => PdfALevel::A1b,
                        1 => PdfALevel::A2b,
                        _ => PdfALevel::A3b,
                    })
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |level: PdfALevel| {
                    app.write_pdf_job("-pdfa", "Archival PDF/A", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform: Transform::ToPdfa(level),
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Report the structural PDF/A problems of the open document.
    pub(super) fn check_pdf_a(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        self.submit("Check PDF/A", "the open document".into(), |job_id| {
            Request::Inspect {
                which: Inspection::PdfA,
                job_id,
            }
        });
    }

    /// Report every digital signature in the open document.
    pub(super) fn verify_signatures(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        self.submit("Verify signatures", "the open document".into(), |job_id| {
            Request::Inspect {
                which: Inspection::Signatures,
                job_id,
            }
        });
    }

    /// Add, remove, or move a bookmark. The numbers are the ones
    /// `foolscap bookmark FILE list` prints.
    pub(super) fn bookmarks(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let count = self.state.borrow().visible_pages();

        show_form(
            &self.ui.window,
            "Bookmarks",
            "Choose where to save…",
            |fields| {
                let mode = dropdown(&["Add", "Remove", "Move up", "Move down"]);
                let title = gtk4::Entry::new();
                title.set_placeholder_text(Some("Chapter 1"));
                let page = gtk4::Entry::new();
                page.set_text("1");
                let parent = gtk4::Entry::new();
                parent.set_text("0");
                let index = gtk4::Entry::new();
                index.set_text("0");
                fields.add("Action", &mode);
                fields.add("Title (add)", &title);
                fields.add("Page (add)", &page);
                fields.add("Nest under (add)", &parent);
                fields.add("Bookmark number", &index);
                fields.note(
                    "Adding uses the title, page and parent. Remove and move use the \
                     bookmark number. Nest under 0 is the top level.",
                );
                Box::new(move || match mode.selected() {
                    0 => {
                        let title = title.text().trim().to_string();
                        if title.is_empty() {
                            return Err("Enter a title for the bookmark.".to_string());
                        }
                        let page: usize = number(&page, "the page number")?;
                        if page == 0 || page > count {
                            return Err(format!("The page must be between 1 and {count}."));
                        }
                        let parent: usize = number(&parent, "the parent number")?;
                        Ok(Transform::Bookmark {
                            title,
                            page: page - 1,
                            parent,
                        })
                    }
                    1 => Ok(Transform::RemoveBookmark(number(
                        &index,
                        "the bookmark number",
                    )?)),
                    choice => Ok(Transform::MoveBookmark {
                        index: number(&index, "the bookmark number")?,
                        up: choice == 2,
                    }),
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |transform: Transform| {
                    app.write_pdf_job("-bookmarked", "Bookmarks", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform,
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Several pages on each sheet, in a grid.
    pub(super) fn n_up(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        show_form(
            &self.ui.window,
            "Pages per sheet",
            "Choose where to save…",
            |fields| {
                let grid = dropdown(&["2 across", "2 down", "2 × 2", "3 × 3"]);
                let paper = dropdown(&paper_labels());
                let gap = gtk4::Entry::new();
                gap.set_text("0");
                fields.add("Layout", &grid);
                fields.add("Paper", &paper);
                fields.add("Gap (points)", &gap);
                Box::new(move || {
                    let (rows, cols) = match grid.selected() {
                        0 => (1, 2),
                        1 => (2, 1),
                        2 => (2, 2),
                        _ => (3, 3),
                    };
                    let gap: f32 = number(&gap, "the gap in points")?;
                    let paper_size =
                        PageSize::parse(paper_at(paper.selected())).map_err(|e| e.to_string())?;
                    let fit = Fit::parse("contain").map_err(|e| e.to_string())?;
                    Ok(NupOptions {
                        rows,
                        cols,
                        paper_size,
                        margins: Margins::default(),
                        gap,
                        border: false,
                        page_order: PageOrder::RowMajor,
                        fit,
                    })
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |options: NupOptions| {
                    app.write_pdf_job("-n-up", "Pages per sheet", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform: Transform::NUp(options),
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Pages imposed for folding into a booklet.
    pub(super) fn booklet(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        show_form(
            &self.ui.window,
            "Booklet",
            "Choose where to save…",
            |fields| {
                let signature = dropdown(&["4 pages per signature", "8 pages", "16 pages"]);
                let paper = dropdown(&paper_labels());
                let binding = dropdown(&["Short edge (landscape sheets)", "Long edge (portrait)"]);
                fields.add("Signature", &signature);
                fields.add("Paper", &paper);
                fields.add("Binding", &binding);
                fields.note("Print double-sided, then fold each sheet into the signature.");
                Box::new(move || {
                    let signature_size = [4, 8, 16][signature.selected() as usize];
                    let paper_size =
                        PageSize::parse(paper_at(paper.selected())).map_err(|e| e.to_string())?;
                    let binding = match binding.selected() {
                        0 => Binding::ShortEdge,
                        _ => Binding::LongEdge,
                    };
                    let fit = Fit::parse("contain").map_err(|e| e.to_string())?;
                    Ok(BookletOptions {
                        signature_size,
                        paper_size,
                        binding,
                        margins: Margins::default(),
                        gap: 0.0,
                        border: false,
                        fit,
                    })
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |options: BookletOptions| {
                    app.write_pdf_job("-booklet", "Booklet", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform: Transform::Booklet(options),
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Cut every page in two.
    pub(super) fn halve(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        show_form(
            &self.ui.window,
            "Halve pages",
            "Choose where to save…",
            |fields| {
                let direction = dropdown(&["Left and right halves", "Top and bottom halves"]);
                let gap = gtk4::Entry::new();
                gap.set_text("0");
                let swap = gtk4::CheckButton::with_label("Put the second half first");
                fields.add("Cut", &direction);
                fields.add("Gutter (points)", &gap);
                fields.wide(&swap);
                Box::new(move || {
                    let direction = match direction.selected() {
                        0 => HalveDirection::Vertical,
                        _ => HalveDirection::Horizontal,
                    };
                    Ok(HalveOptions {
                        direction,
                        gap: number(&gap, "the gutter in points")?,
                        swap: swap.is_active(),
                    })
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |options: HalveOptions| {
                    app.write_pdf_job("-halved", "Halve pages", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform: Transform::Halve(options),
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Fill the open form from a JSON file of values.
    pub(super) fn fill_form(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        let dialog = FileDialog::builder()
            .title("Choose the JSON file of values")
            .build();
        dialog.open(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(file) = result else { return };
                    let Some(data_path) = file.path() else {
                        return;
                    };
                    app.write_pdf_job("-filled", "Fill form", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform: Transform::FillForm(data_path),
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// A blank document of chosen size and page count, saved to a new file.
    pub(super) fn new_blank(self: &Rc<Self>) {
        show_form(
            &self.ui.window,
            "New blank PDF",
            "Choose where to save…",
            |fields| {
                let pages = gtk4::Entry::new();
                pages.set_text("1");
                let paper = dropdown(&paper_labels());
                fields.add("Pages", &pages);
                fields.add("Paper", &paper);
                Box::new(move || {
                    let pages: usize = number(&pages, "the number of pages")?;
                    if pages == 0 {
                        return Err("A document needs at least one page.".to_string());
                    }
                    let size =
                        PageSize::parse(paper_at(paper.selected())).map_err(|e| e.to_string())?;
                    Ok(Creation::Blank { pages, size })
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |creation: Creation| {
                    app.ask_for_file("blank.pdf", move |app, destination| {
                        let detail = destination.display().to_string();
                        app.submit("New blank PDF", detail, move |job_id| Request::Create {
                            path: destination,
                            creation,
                            job_id,
                        });
                    });
                }
            ),
        );
    }

    /// A one-page PDF holding a QR code.
    pub(super) fn qr_code(self: &Rc<Self>) {
        show_form(
            &self.ui.window,
            "QR code",
            "Choose where to save…",
            |fields| {
                let text = gtk4::Entry::new();
                text.set_placeholder_text(Some("https://example.com"));
                let size = gtk4::Entry::new();
                size.set_text("40");
                let ecc = dropdown(&["Low", "Medium", "Quartile", "High"]);
                let label = gtk4::Entry::new();
                label.set_placeholder_text(Some("Optional caption"));
                fields.add("Text", &text);
                fields.add("Size (mm)", &size);
                fields.add("Error correction", &ecc);
                fields.add("Label", &label);
                Box::new(move || {
                    let text = text.text().to_string();
                    if text.trim().is_empty() {
                        return Err("Enter the text the code should hold.".to_string());
                    }
                    let size_mm: f64 = number(&size, "the size in millimetres")?;
                    let ecc = match ecc.selected() {
                        0 => QrEcc::Low,
                        1 => QrEcc::Medium,
                        2 => QrEcc::Quartile,
                        _ => QrEcc::High,
                    };
                    let caption = label.text().trim().to_string();
                    let options = QrOptions {
                        size_mm,
                        ecc,
                        label: (!caption.is_empty()).then_some(caption),
                    };
                    Ok(Creation::Qr { text, options })
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |creation: Creation| {
                    app.ask_for_file("qr.pdf", move |app, destination| {
                        let detail = destination.display().to_string();
                        app.submit("QR code", detail, move |job_id| Request::Create {
                            path: destination,
                            creation,
                            job_id,
                        });
                    });
                }
            ),
        );
    }

    /// One SVG per page, into a folder. Works on the saved file, so unsaved
    /// edits are refused rather than silently left out.
    pub(super) fn to_svg(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        if self.state.borrow().is_dirty() {
            self.set_status("Save the document first, so the SVG files match what you see.");
            return;
        }
        let count = self.state.borrow().visible_pages();

        show_form(
            &self.ui.window,
            "Pages as SVG",
            "Choose the folder…",
            |fields| {
                let pages = pages_entry();
                fields.add("Pages", &pages);
                fields.note("Each page becomes a vector file. The text is drawn as paths, so it cannot be selected in the SVG.");
                Box::new(move || pages_from(&pages, count))
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |pages: PageRange| {
                    app.ask_for_folder(
                        "Choose the folder for the SVG files",
                        move |app, folder| {
                            let detail = folder.display().to_string();
                            app.submit("Pages as SVG", detail, move |job_id| Request::ExportSvg {
                                out_dir: folder,
                                pages,
                                job_id,
                            });
                        },
                    );
                }
            ),
        );
    }

    /// The document's text, structured as Markdown.
    pub(super) fn to_markdown(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        self.ask_for_file("document.md", move |app, output| {
            let edits = app.state.borrow().edits();
            let detail = output.display().to_string();
            app.submit("Export Markdown", detail, move |job_id| {
                Request::ExportMarkdown {
                    path: output,
                    edits,
                    job_id,
                }
            });
        });
    }
}

impl App {
    /// Embed a chosen file in the open document.
    pub(super) fn attach_file(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let dialog = FileDialog::builder()
            .title("Choose the file to attach")
            .build();
        dialog.open(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(file) = result else { return };
                    let Some(attachment) = file.path() else {
                        return;
                    };
                    app.write_pdf_job("-attached", "Attach file", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform: Transform::Attach(attachment),
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Show or hide one optional content layer, by its name.
    pub(super) fn layers(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        show_form(
            &self.ui.window,
            "Layers",
            "Choose where to save…",
            |fields| {
                let name = gtk4::Entry::new();
                name.set_placeholder_text(Some("Layer name, as the viewer shows it"));
                let action = dropdown(&["Show", "Hide"]);
                fields.add("Layer", &name);
                fields.add("Action", &action);
                fields.note("Layer names are the ones `foolscap layers FILE list` prints.");
                Box::new(move || {
                    let name = name.text().trim().to_string();
                    if name.is_empty() {
                        return Err("Enter the name of the layer.".to_string());
                    }
                    Ok((name, action.selected() == 0))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(name, visible): (String, bool)| {
                    app.write_pdf_job("-layers", "Layers", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform: Transform::SetLayer { name, visible },
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Number the pages in a chosen style, or remove the numbering.
    pub(super) fn page_labels(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        show_form(
            &self.ui.window,
            "Page labels",
            "Choose where to save…",
            |fields| {
                let style = dropdown(&[
                    "Numbers: 1, 2, 3",
                    "Lower roman: i, ii, iii",
                    "Upper roman: I, II, III",
                    "Upper letters: A, B, C",
                    "Lower letters: a, b, c",
                    "Prefix only",
                    "No labels (remove them)",
                ]);
                let prefix = gtk4::Entry::new();
                prefix.set_placeholder_text(Some("Optional, e.g. A-"));
                let first = gtk4::Entry::new();
                first.set_text("1");
                fields.add("Style", &style);
                fields.add("Prefix", &prefix);
                fields.add("First number", &first);
                fields.note("The labels apply to every page, starting at page 1.");
                Box::new(move || {
                    const STYLES: [Option<pdf_core::LabelStyle>; 6] = [
                        Some(pdf_core::LabelStyle::Decimal),
                        Some(pdf_core::LabelStyle::LowerRoman),
                        Some(pdf_core::LabelStyle::UpperRoman),
                        Some(pdf_core::LabelStyle::UpperAlpha),
                        Some(pdf_core::LabelStyle::LowerAlpha),
                        Some(pdf_core::LabelStyle::None),
                    ];
                    // The last choice, with no entry in STYLES, removes the labels.
                    let style = match STYLES.get(style.selected() as usize) {
                        Some(Some(style)) => *style,
                        _ => return Ok(Vec::new()),
                    };
                    let first: u32 = number(&first, "the first number")?;
                    if first == 0 {
                        return Err("The first number is at least 1.".to_string());
                    }
                    Ok(vec![pdf_core::LabelRange {
                        start_page: 1,
                        style,
                        prefix: prefix.text().to_string(),
                        first,
                    }])
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |ranges: Vec<pdf_core::LabelRange>| {
                    app.write_pdf_job("-labelled", "Page labels", move |path, edits, job_id| {
                        Request::Transform {
                            path,
                            transform: Transform::PageLabels(ranges),
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }
}

impl App {
    /// List the embedded files of the open document.
    pub(super) fn list_attachments(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        self.submit("List attachments", "the open document".into(), |job_id| {
            Request::Inspect {
                which: Inspection::Attachments,
                job_id,
            }
        });
    }

    /// List the optional content layers of the open document.
    pub(super) fn list_layers(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        self.submit("List layers", "the open document".into(), |job_id| {
            Request::Inspect {
                which: Inspection::Layers,
                job_id,
            }
        });
    }
}
