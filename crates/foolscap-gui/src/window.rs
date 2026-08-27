//! The main window.
//!
//! The layout is a thumbnail sidebar beside a page view, with the actions on a
//! header bar. Every slow call goes to the worker thread and comes back as a
//! message, so the interface stays responsive while a document is being
//! recognised or compressed.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

use gtk4::gdk;
use gtk4::gio;
use gtk4::glib::{self, clone};
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, DropTarget, FileDialog, FlowBox,
    HeaderBar, Label, MenuButton, Orientation, Paned, Picture, PopoverMenu, ProgressBar,
    ScrolledWindow, SelectionMode, Separator,
};
use pdf_core::render::Scale;
use pdf_core::CompressLevel;

use crate::image::texture;
use crate::state::State;
use crate::worker::{self, Request, Response};

/// Longest edge of a sidebar thumbnail, in pixels.
const THUMBNAIL_SIZE: u32 = 140;

/// Resolution for the main page view.
const VIEW_DPI: f32 = 110.0;

struct Ui {
    window: ApplicationWindow,
    thumbnails: FlowBox,
    page_view: Picture,
    status: Label,
    progress: ProgressBar,
    subtitle: Label,
}

struct App {
    ui: Ui,
    state: RefCell<State>,
    to_worker: mpsc::Sender<Request>,
    /// Incremented whenever the document changes, so that renders requested for
    /// an older document can be recognised and thrown away when they arrive.
    generation: RefCell<u64>,
    busy: RefCell<bool>,
    /// One per visible position. Held directly because reaching them by walking
    /// the widget tree depends on the exact nesting, which is easy to get
    /// wrong and fails silently when it is.
    thumbnail_pictures: RefCell<Vec<Picture>>,
}

pub fn build(app: &Application, open: Option<PathBuf>) {
    let (to_worker, from_worker) = worker::spawn();

    let thumbnails = FlowBox::builder()
        .selection_mode(SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(1)
        .row_spacing(8)
        .column_spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();

    let sidebar = ScrolledWindow::builder()
        .child(&thumbnails)
        .width_request(190)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .build();

    let page_view = Picture::builder()
        .can_shrink(true)
        .halign(Align::Center)
        .valign(Align::Center)
        .build();

    let viewer = ScrolledWindow::builder().child(&page_view).build();

    let paned = Paned::builder()
        .orientation(Orientation::Horizontal)
        .start_child(&sidebar)
        .end_child(&viewer)
        .resize_start_child(false)
        .shrink_start_child(false)
        .build();

    let status = Label::builder()
        .label("Open a PDF to begin.")
        .xalign(0.0)
        .margin_start(8)
        .margin_end(8)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();

    let progress = ProgressBar::builder().visible(false).build();

    let status_bar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    status_bar.append(&status);
    status_bar.append(&progress);

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.append(&paned);
    content.append(&Separator::new(Orientation::Horizontal));
    content.append(&status_bar);
    paned.set_vexpand(true);

    let subtitle = Label::builder()
        .label("No document")
        .css_classes(vec!["dim-label".to_string()])
        .build();

    let title = GtkBox::new(Orientation::Vertical, 0);
    let name = Label::builder()
        .label("Foolscap")
        .css_classes(vec!["title".to_string()])
        .build();
    title.append(&name);
    title.append(&subtitle);

    let header = HeaderBar::builder().title_widget(&title).build();

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Foolscap")
        .default_width(1100)
        .default_height(760)
        .child(&content)
        .build();
    window.set_titlebar(Some(&header));

    let app_state = Rc::new(App {
        ui: Ui {
            window: window.clone(),
            thumbnails: thumbnails.clone(),
            page_view: page_view.clone(),
            status: status.clone(),
            progress: progress.clone(),
            subtitle: subtitle.clone(),
        },
        state: RefCell::new(State::default()),
        to_worker,
        generation: RefCell::new(0),
        busy: RefCell::new(false),
        thumbnail_pictures: RefCell::new(Vec::new()),
    });

    // Let the worker thread finish rather than leaving it parked on a channel
    // that will never receive anything again.
    window.connect_close_request(clone!(
        #[strong]
        app_state,
        move |_| {
            let _ = app_state.to_worker.send(Request::Quit);
            glib::Propagation::Proceed
        }
    ));

    add_header_buttons(&header, &app_state);
    add_drop_target(&window, &app_state);
    listen(&app_state, from_worker);

    tracing::info!("presenting the window");
    window.present();

    if let Some(path) = open {
        app_state.open(path);
    }
}

fn add_header_buttons(header: &HeaderBar, app: &Rc<App>) {
    let open = Button::from_icon_name("document-open-symbolic");
    open.set_tooltip_text(Some("Open a PDF"));
    open.connect_clicked(clone!(
        #[strong]
        app,
        move |_| app.choose_file_to_open()
    ));
    header.pack_start(&open);

    let rotate_left = Button::from_icon_name("object-rotate-left-symbolic");
    rotate_left.set_tooltip_text(Some("Rotate anticlockwise"));
    rotate_left.connect_clicked(clone!(
        #[strong]
        app,
        move |_| app.rotate(-90)
    ));
    header.pack_start(&rotate_left);

    let rotate_right = Button::from_icon_name("object-rotate-right-symbolic");
    rotate_right.set_tooltip_text(Some("Rotate clockwise"));
    rotate_right.connect_clicked(clone!(
        #[strong]
        app,
        move |_| app.rotate(90)
    ));
    header.pack_start(&rotate_right);

    let delete = Button::from_icon_name("user-trash-symbolic");
    delete.set_tooltip_text(Some("Delete the selected pages"));
    delete.connect_clicked(clone!(
        #[strong]
        app,
        move |_| app.delete_pages()
    ));
    header.pack_start(&delete);

    let save = Button::from_icon_name("document-save-as-symbolic");
    save.set_tooltip_text(Some("Save as"));
    save.connect_clicked(clone!(
        #[strong]
        app,
        move |_| app.save_as()
    ));
    header.pack_end(&save);

    let menu = gio::Menu::new();
    let export = gio::Menu::new();
    export.append(Some("Compress for screen"), Some("win.compress-screen"));
    export.append(Some("Compress for print"), Some("win.compress-print"));
    export.append(Some("Export pages as images"), Some("win.export-images"));
    menu.append_section(None, &export);

    let text = gio::Menu::new();
    text.append(Some("Make searchable (OCR)"), Some("win.ocr"));
    menu.append_section(None, &text);

    let more = MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .popover(&PopoverMenu::from_model(Some(&menu)))
        .build();
    header.pack_end(&more);

    add_action(&app.ui.window, "compress-screen", app, |app| {
        app.compress(CompressLevel::Screen)
    });
    add_action(&app.ui.window, "compress-print", app, |app| {
        app.compress(CompressLevel::Print)
    });
    add_action(&app.ui.window, "export-images", app, |app| {
        app.export_images()
    });
    add_action(&app.ui.window, "ocr", app, |app| app.run_ocr());
}

fn add_action(
    window: &ApplicationWindow,
    name: &str,
    app: &Rc<App>,
    handler: impl Fn(&Rc<App>) + 'static,
) {
    let action = gio::SimpleAction::new(name, None);
    action.connect_activate(clone!(
        #[strong]
        app,
        move |_, _| handler(&app)
    ));
    window.add_action(&action);
}

/// Accept PDFs dropped onto the window: one replaces what is open, several are
/// merged in the order they arrive.
fn add_drop_target(window: &ApplicationWindow, app: &Rc<App>) {
    let target = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);

    target.connect_drop(clone!(
        #[strong]
        app,
        move |_, value, _, _| {
            let Ok(files) = value.get::<gdk::FileList>() else {
                return false;
            };

            let paths: Vec<PathBuf> = files
                .files()
                .iter()
                .filter_map(|file| file.path())
                .collect();

            match paths.len() {
                0 => false,
                1 => {
                    app.open(paths.into_iter().next().unwrap());
                    true
                }
                _ => {
                    app.merge_dropped(paths);
                    true
                }
            }
        }
    ));

    window.add_controller(target);
}

/// Feed worker replies into the main loop.
fn listen(app: &Rc<App>, from_worker: async_channel::Receiver<Response>) {
    glib::spawn_future_local(clone!(
        #[strong]
        app,
        async move {
            while let Ok(response) = from_worker.recv().await {
                app.handle(response);
            }
        }
    ));
}

impl App {
    fn open(self: &Rc<Self>, path: PathBuf) {
        self.set_status(&format!("Opening {}…", path.display()));
        let _ = self.to_worker.send(Request::Open(path));
    }

    fn handle(self: &Rc<Self>, response: Response) {
        match response {
            Response::Opened { path, page_count } => {
                *self.generation.borrow_mut() += 1;
                self.state.borrow_mut().open(path.clone(), page_count);

                self.ui.subtitle.set_label(&format!(
                    "{} — {} page{}",
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    page_count,
                    if page_count == 1 { "" } else { "s" }
                ));

                self.rebuild_thumbnails();
                self.show_current();
                self.set_status("Ready.");
            }

            Response::Rendered {
                page,
                generation,
                image,
            } => {
                // A render for a document that has since been replaced.
                if generation != *self.generation.borrow() {
                    return;
                }
                self.place_render(page, &image);
            }

            Response::Progress(message) => {
                self.ui.progress.set_visible(true);
                self.ui.progress.pulse();
                self.set_status(&message);
            }

            Response::Finished { message } => {
                *self.busy.borrow_mut() = false;
                self.ui.progress.set_visible(false);
                self.set_status(&message);
            }

            Response::Failed(message) => {
                *self.busy.borrow_mut() = false;
                self.ui.progress.set_visible(false);
                self.set_status(&format!("Failed: {message}"));
            }
        }
    }

    /// Put a rendered page where it belongs: the main view, a thumbnail, or
    /// both, depending on which positions currently show that source page.
    fn place_render(self: &Rc<Self>, page: usize, image: &pdf_core::render::RenderedPage) {
        let state = self.state.borrow();

        for (position, source) in state.order.iter().enumerate() {
            if *source != page {
                continue;
            }

            let rotation = state.rotation_at(position);
            let texture = texture(image, rotation);

            if position == state.current && image.width > THUMBNAIL_SIZE {
                self.ui.page_view.set_paintable(Some(&texture));
            }

            if image.width <= THUMBNAIL_SIZE * 2 {
                if let Some(picture) = self.thumbnail_pictures.borrow().get(position) {
                    picture.set_paintable(Some(&texture));
                }
            }
        }
    }

    fn rebuild_thumbnails(self: &Rc<Self>) {
        while let Some(child) = self.ui.thumbnails.first_child() {
            self.ui.thumbnails.remove(&child);
        }
        self.thumbnail_pictures.borrow_mut().clear();

        let (visible, generation) = {
            let state = self.state.borrow();
            (state.visible_pages(), *self.generation.borrow())
        };

        for position in 0..visible {
            let picture = Picture::builder()
                .height_request(THUMBNAIL_SIZE as i32)
                .can_shrink(true)
                .build();

            let number = Label::builder()
                .label(format!("{}", position + 1))
                .css_classes(vec!["dim-label".to_string()])
                .build();

            let item = GtkBox::new(Orientation::Vertical, 2);
            item.append(&picture);
            item.append(&number);

            attach_thumbnail_gestures(&item, position, self);

            self.ui.thumbnails.insert(&item, position as i32);
            self.thumbnail_pictures.borrow_mut().push(picture);

            let page = self.state.borrow().source_page(position);
            if let Some(page) = page {
                let _ = self.to_worker.send(Request::Render {
                    page,
                    scale: Scale::FitBox(THUMBNAIL_SIZE),
                    generation,
                });
            }
        }

        self.refresh_selection();
    }

    fn refresh_selection(self: &Rc<Self>) {
        let state = self.state.borrow();

        for position in 0..state.visible_pages() {
            let Some(child) = self.ui.thumbnails.child_at_index(position as i32) else {
                continue;
            };

            let selected = state.selection.contains(&position) || state.current == position;
            if selected {
                child.add_css_class("view");
                child.set_opacity(1.0);
            } else {
                child.remove_css_class("view");
                child.set_opacity(0.75);
            }
        }
    }

    fn show_current(self: &Rc<Self>) {
        let (page, generation) = {
            let state = self.state.borrow();
            (state.source_page(state.current), *self.generation.borrow())
        };

        let Some(page) = page else {
            self.ui.page_view.set_paintable(None::<&gdk::Texture>);
            return;
        };

        let _ = self.to_worker.send(Request::Render {
            page,
            scale: Scale::Dpi(VIEW_DPI),
            generation,
        });
    }

    fn rotate(self: &Rc<Self>, degrees: i32) {
        if !self.require_document() {
            return;
        }

        self.state.borrow_mut().rotate(degrees);
        self.rebuild_thumbnails();
        self.show_current();
        self.set_status("Rotated. Save to write the change out.");
    }

    fn delete_pages(self: &Rc<Self>) {
        if !self.require_document() {
            return;
        }

        let outcome = self.state.borrow_mut().delete();
        match outcome {
            Ok(()) => {
                self.rebuild_thumbnails();
                self.show_current();
                self.set_status("Deleted. Save to write the change out.");
            }
            Err(reason) => self.set_status(&format!("Cannot delete: {reason}")),
        }
    }

    fn select(self: &Rc<Self>, position: usize, extend: bool) {
        {
            let mut state = self.state.borrow_mut();
            if extend {
                state.toggle_selection(position);
            } else {
                state.select_only(position);
            }
        }

        self.refresh_selection();
        self.show_current();
    }

    fn move_page(self: &Rc<Self>, from: usize, to: usize) {
        self.state.borrow_mut().move_page(from, to);
        self.rebuild_thumbnails();
        self.show_current();
        self.set_status("Reordered. Save to write the change out.");
    }

    fn merge_dropped(self: &Rc<Self>, paths: Vec<PathBuf>) {
        // Merging writes a new file, so it needs a destination first.
        let dialog = FileDialog::builder()
            .title("Save the merged document")
            .initial_name("merged.pdf")
            .build();

        dialog.save(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(file) = result else { return };
                    let Some(destination) = file.path() else {
                        return;
                    };

                    app.set_status("Merging…");
                    match pdf_core::merge(&paths, &destination) {
                        Ok(()) => app.open(destination),
                        Err(error) => app.set_status(&format!("Failed: {error}")),
                    }
                }
            ),
        );
    }

    fn choose_file_to_open(self: &Rc<Self>) {
        let filter = gtk4::FileFilter::new();
        filter.set_name(Some("PDF documents"));
        filter.add_mime_type("application/pdf");
        filter.add_pattern("*.pdf");

        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);

        let dialog = FileDialog::builder()
            .title("Open a PDF")
            .filters(&filters)
            .build();

        dialog.open(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            app.open(path);
                        }
                    }
                }
            ),
        );
    }

    fn save_as(self: &Rc<Self>) {
        if !self.require_document() {
            return;
        }

        let name = self.state.borrow().suggested_name("-edited", "pdf");
        self.ask_for_file(&name, move |app, destination| {
            let edits = app.state.borrow().edits();
            app.start("Saving…");
            let _ = app.to_worker.send(Request::Save {
                path: destination,
                edits,
            });
        });
    }

    fn compress(self: &Rc<Self>, level: CompressLevel) {
        if !self.require_document() {
            return;
        }

        let name = self.state.borrow().suggested_name("-compressed", "pdf");
        self.ask_for_file(&name, move |app, destination| {
            let edits = app.state.borrow().edits();
            app.start("Compressing…");
            let _ = app.to_worker.send(Request::Compress {
                path: destination,
                level,
                edits,
            });
        });
    }

    fn run_ocr(self: &Rc<Self>) {
        if !self.require_document() {
            return;
        }

        if !pdf_core::ocr::is_available() {
            self.set_status("Tesseract is not installed; run: sudo apt install tesseract-ocr");
            return;
        }

        let name = self.state.borrow().suggested_name("-searchable", "pdf");
        self.ask_for_file(&name, move |app, destination| {
            let edits = app.state.borrow().edits();
            app.start("Recognising text…");
            let _ = app.to_worker.send(Request::Ocr {
                path: destination,
                language: "eng".to_string(),
                edits,
            });
        });
    }

    fn export_images(self: &Rc<Self>) {
        if !self.require_document() {
            return;
        }

        let dialog = FileDialog::builder()
            .title("Choose a folder for the images")
            .build();

        dialog.select_folder(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(folder) = result else { return };
                    let Some(directory) = folder.path() else {
                        return;
                    };

                    let edits = app.state.borrow().edits();
                    app.start("Exporting images…");
                    let _ = app.to_worker.send(Request::ExportImages {
                        directory,
                        dpi: 150.0,
                        edits,
                    });
                }
            ),
        );
    }

    fn ask_for_file(self: &Rc<Self>, suggested: &str, then: impl Fn(&Rc<Self>, PathBuf) + 'static) {
        let dialog = FileDialog::builder()
            .title("Save as")
            .initial_name(suggested)
            .build();

        dialog.save(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            then(&app, path);
                        }
                    }
                }
            ),
        );
    }

    /// Refuse to start a second long operation while one is running: the worker
    /// is a single thread, and queueing them silently would look like a hang.
    fn start(self: &Rc<Self>, message: &str) {
        *self.busy.borrow_mut() = true;
        self.ui.progress.set_visible(true);
        self.ui.progress.pulse();
        self.set_status(message);
    }

    fn require_document(self: &Rc<Self>) -> bool {
        if *self.busy.borrow() {
            self.set_status("Still working on the last request…");
            return false;
        }

        if !self.state.borrow().is_open() {
            self.set_status("Open a PDF first.");
            return false;
        }

        true
    }

    fn set_status(&self, message: &str) {
        self.ui.status.set_label(message);
    }
}

/// Click to select, control-click to extend, and drag to reorder.
fn attach_thumbnail_gestures(item: &GtkBox, position: usize, app: &Rc<App>) {
    let click = gtk4::GestureClick::new();
    click.connect_pressed(clone!(
        #[strong]
        app,
        move |gesture, _, _, _| {
            let extend = gesture
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK);
            app.select(position, extend);
        }
    ));
    item.add_controller(click);

    let source = gtk4::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    source.connect_prepare(move |_, _, _| {
        // The payload is the position being dragged; the drop target turns it
        // back into a move.
        Some(gdk::ContentProvider::for_value(&(position as u32).into()))
    });
    item.add_controller(source);

    let target = DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
    target.connect_drop(clone!(
        #[strong]
        app,
        move |_, value, _, _| {
            let Ok(from) = value.get::<u32>() else {
                return false;
            };
            app.move_page(from as usize, position);
            true
        }
    ));
    item.add_controller(target);
}
