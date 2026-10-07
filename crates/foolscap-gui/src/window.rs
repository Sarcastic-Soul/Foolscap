//! The main window.
//!
//! The layout is a thumbnail sidebar beside a page view, with the actions on a
//! header bar. Every slow call goes to the worker thread and comes back as a
//! message, so the interface stays responsive while a document is being
//! recognised or compressed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk4::gdk;
use gtk4::gio;
use gtk4::glib::{self, clone};
use gtk4::prelude::*;
use gtk4::{
    AlertDialog, Align, Application, ApplicationWindow, Box as GtkBox, Button, DropTarget,
    FileDialog, FlowBox, HeaderBar, Label, MenuButton, Orientation, Paned, Picture, PopoverMenu,
    ProgressBar, ScrolledWindow, SelectionMode, Separator,
};
use pdf_core::{CompressLevel, PdfError};

use crate::image::texture;
use crate::state::State;
use crate::worker::{self, Edits, Purpose, Request, Response, Size, Task};

/// Longest edge of a sidebar thumbnail, in pixels.
const THUMBNAIL_SIZE: u32 = 140;

/// Resolution that counts as 100% zoom: one CSS pixel per pixel.
const ZOOM_100_DPI: f32 = 96.0;

/// Space around the page in the main view, in pixels.
const VIEW_MARGIN: i32 = 12;

/// Zoom levels the zoom buttons step through.
const ZOOM_STEPS: [f32; 11] = [0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];

/// Keyboard shortcuts, as (action, accelerators). The shortcuts window below
/// lists the same ones; keep the two in step.
const ACCELS: &[(&str, &[&str])] = &[
    ("win.open", &["<Primary>o"]),
    ("win.save", &["<Primary>s"]),
    ("win.save-as", &["<Primary><Shift>s"]),
    ("win.close", &["<Primary>w"]),
    ("app.quit", &["<Primary>q"]),
    ("win.undo", &["<Primary>z"]),
    ("win.redo", &["<Primary><Shift>z", "<Primary>y"]),
    ("win.select-all", &["<Primary>a"]),
    ("win.delete", &["Delete", "KP_Delete"]),
    ("win.rotate-left", &["<Primary>bracketleft", "<Primary>l"]),
    ("win.rotate-right", &["<Primary>bracketright", "<Primary>r"]),
    (
        "win.zoom-in",
        &["<Primary>plus", "<Primary>equal", "<Primary>KP_Add"],
    ),
    ("win.zoom-out", &["<Primary>minus", "<Primary>KP_Subtract"]),
    ("win.zoom-fit", &["<Primary>0", "<Primary>KP_0"]),
    ("win.shortcuts", &["<Primary>question"]),
];

/// Actions that need an open document and an idle worker.
const DOCUMENT_ACTIONS: &[&str] = &[
    "save-as",
    "rotate-left",
    "rotate-right",
    "delete",
    "select-all",
    "compress-screen",
    "compress-print",
    "export-images",
    "ocr",
];

/// Install what belongs to the application rather than to one window: the
/// quit action and every keyboard shortcut.
pub fn install_app_actions(app: &Application) {
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(clone!(
        #[weak]
        app,
        move |_, _| {
            // Closing each window, rather than quitting outright, gives every
            // window the chance to ask about unsaved changes.
            for window in app.windows() {
                window.close();
            }
        }
    ));
    app.add_action(&quit);

    for (action, accels) in ACCELS {
        app.set_accels_for_action(action, accels);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Zoom {
    FitWidth,
    Factor(f32),
}

/// What to do with the document the worker reports as opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenMode {
    /// A new document: forget history.
    Fresh,
    /// The result of merging dropped files: keep the way back.
    Merge,
    /// Reopening an earlier document to step back or forward to it.
    Undo,
    Redo,
}

type Continuation = Box<dyn FnOnce(&Rc<App>)>;

struct Ui {
    window: ApplicationWindow,
    thumbnails: FlowBox,
    viewer: ScrolledWindow,
    page_view: Picture,
    status: Label,
    progress: ProgressBar,
    subtitle: Label,
    zoom_level: Label,
}

struct App {
    ui: Ui,
    state: RefCell<State>,
    to_worker: mpsc::Sender<Request>,
    /// Incremented whenever the document changes, so that renders requested for
    /// an older document can be recognised and thrown away when they arrive.
    generation: Cell<u64>,
    busy: Cell<bool>,
    /// One per visible position. Held directly because reaching them by walking
    /// the widget tree depends on the exact nesting, which is easy to get
    /// wrong and fails silently when it is.
    thumbnail_pictures: RefCell<Vec<Picture>>,
    /// Window actions by name, so their enabled state can follow the document.
    actions: RefCell<HashMap<&'static str, gio::SimpleAction>>,
    zoom: Cell<Zoom>,
    /// Resolution of the page currently in the main view.
    view_dpi: Cell<f32>,
    /// The latest main-view render asked for; older ones are ignored.
    view_ticket: Cell<u64>,
    /// A pending re-render after the window was resized.
    refit: RefCell<Option<glib::SourceId>>,
    open_mode: Cell<OpenMode>,
    /// The edits sent with the save in progress, which become the saved state
    /// once the worker confirms.
    saving: RefCell<Option<Edits>>,
    /// What to do once the save in progress succeeds, such as closing.
    after_save: RefCell<Option<Continuation>>,
    /// Set once the user has agreed to close despite unsaved changes.
    close_confirmed: Cell<bool>,
    /// The inputs and output of the merge in progress, kept so it can be tried
    /// again once a protected input is unlocked.
    pending_merge: RefCell<Option<(Vec<PathBuf>, PathBuf)>>,
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

    // The picture is shown at the size it was rendered, so zooming is a matter
    // of rendering at a different resolution and scrolling does the rest.
    let page_view = Picture::builder()
        .can_shrink(false)
        .halign(Align::Center)
        .valign(Align::Center)
        .margin_top(VIEW_MARGIN)
        .margin_bottom(VIEW_MARGIN)
        .margin_start(VIEW_MARGIN)
        .margin_end(VIEW_MARGIN)
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
        .hexpand(true)
        .margin_start(8)
        .margin_end(8)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();

    let progress = ProgressBar::builder()
        .visible(false)
        .valign(Align::Center)
        .build();

    let zoom_level = Label::builder()
        .label("100%")
        .width_chars(5)
        .css_classes(vec!["dim-label".to_string()])
        .build();

    let zoom_box = GtkBox::new(Orientation::Horizontal, 0);
    zoom_box.set_margin_end(4);
    zoom_box.append(&flat_button(
        "zoom-out-symbolic",
        "Zoom out (Ctrl+−)",
        "win.zoom-out",
    ));
    zoom_box.append(&zoom_level);
    zoom_box.append(&flat_button(
        "zoom-in-symbolic",
        "Zoom in (Ctrl++)",
        "win.zoom-in",
    ));
    zoom_box.append(&flat_button(
        "zoom-fit-best-symbolic",
        "Fit the page to the window width (Ctrl+0)",
        "win.zoom-fit",
    ));

    let status_bar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .margin_top(2)
        .margin_bottom(2)
        .build();
    status_bar.append(&status);
    status_bar.append(&progress);
    status_bar.append(&zoom_box);

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.append(&paned);
    content.append(&Separator::new(Orientation::Horizontal));
    content.append(&status_bar);
    paned.set_vexpand(true);

    let subtitle = Label::builder()
        .label("No document")
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .css_classes(vec!["dim-label".to_string()])
        .build();

    let title = GtkBox::new(Orientation::Vertical, 0);
    title.set_valign(Align::Center);
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
    if let Some(icon) = installed_icon() {
        window.set_icon_name(Some(icon));
    }

    let app_state = Rc::new(App {
        ui: Ui {
            window: window.clone(),
            thumbnails: thumbnails.clone(),
            viewer: viewer.clone(),
            page_view: page_view.clone(),
            status: status.clone(),
            progress: progress.clone(),
            subtitle: subtitle.clone(),
            zoom_level: zoom_level.clone(),
        },
        state: RefCell::new(State::default()),
        to_worker,
        generation: Cell::new(0),
        busy: Cell::new(false),
        thumbnail_pictures: RefCell::new(Vec::new()),
        actions: RefCell::new(HashMap::new()),
        zoom: Cell::new(Zoom::FitWidth),
        view_dpi: Cell::new(ZOOM_100_DPI),
        view_ticket: Cell::new(0),
        refit: RefCell::new(None),
        open_mode: Cell::new(OpenMode::Fresh),
        saving: RefCell::new(None),
        after_save: RefCell::new(None),
        close_confirmed: Cell::new(false),
        pending_merge: RefCell::new(None),
    });

    window.connect_close_request(clone!(
        #[strong]
        app_state,
        move |_| {
            let unsaved = app_state.state.borrow().is_dirty();
            if unsaved && !app_state.close_confirmed.get() {
                app_state.confirm_discard(|app| {
                    app.close_confirmed.set(true);
                    app.ui.window.close();
                });
                return glib::Propagation::Stop;
            }

            // Let the worker thread finish rather than leaving it parked on a
            // channel that will never receive anything again.
            let _ = app_state.to_worker.send(Request::Quit);
            glib::Propagation::Proceed
        }
    ));

    add_actions(&app_state);
    add_header_buttons(&header);
    add_drop_target(&window, &app_state);
    add_view_controllers(&app_state);
    listen(&app_state, from_worker);
    app_state.refresh_title();
    app_state.refresh_actions();

    tracing::info!("presenting the window");
    window.present();

    if let Some(path) = open {
        app_state.open(path);
    }
}

/// The icon the packages install, if this system has it.
fn installed_icon() -> Option<&'static str> {
    let display = gdk::Display::default()?;
    let theme = gtk4::IconTheme::for_display(&display);
    [crate::APP_ID, "foolscap"]
        .into_iter()
        .find(|name| theme.has_icon(name))
}

fn flat_button(icon: &str, tooltip: &str, action: &str) -> Button {
    let button = Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.set_action_name(Some(action));
    button.add_css_class("flat");
    button
}

fn header_button(icon: &str, tooltip: &str, action: &str) -> Button {
    let button = Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.set_action_name(Some(action));
    button
}

fn add_header_buttons(header: &HeaderBar) {
    header.pack_start(&header_button(
        "document-open-symbolic",
        "Open a PDF (Ctrl+O)",
        "win.open",
    ));
    header.pack_start(&header_button(
        "edit-undo-symbolic",
        "Undo (Ctrl+Z)",
        "win.undo",
    ));
    header.pack_start(&header_button(
        "edit-redo-symbolic",
        "Redo (Ctrl+Shift+Z)",
        "win.redo",
    ));
    header.pack_start(&header_button(
        "object-rotate-left-symbolic",
        "Rotate anticlockwise (Ctrl+[)",
        "win.rotate-left",
    ));
    header.pack_start(&header_button(
        "object-rotate-right-symbolic",
        "Rotate clockwise (Ctrl+])",
        "win.rotate-right",
    ));
    header.pack_start(&header_button(
        "user-trash-symbolic",
        "Delete the selected pages (Delete)",
        "win.delete",
    ));

    let menu = gio::Menu::new();

    let file = gio::Menu::new();
    file.append(Some("Save As…"), Some("win.save-as"));
    menu.append_section(None, &file);

    let export = gio::Menu::new();
    export.append(Some("Compress for screen"), Some("win.compress-screen"));
    export.append(Some("Compress for print"), Some("win.compress-print"));
    export.append(Some("Export pages as images"), Some("win.export-images"));
    menu.append_section(None, &export);

    let text = gio::Menu::new();
    text.append(Some("Make searchable (OCR)"), Some("win.ocr"));
    menu.append_section(None, &text);

    let help = gio::Menu::new();
    help.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
    help.append(Some("About Foolscap"), Some("win.about"));
    menu.append_section(None, &help);

    let more = MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main menu")
        .popover(&PopoverMenu::from_model(Some(&menu)))
        .build();
    header.pack_end(&more);

    header.pack_end(&header_button(
        "document-save-symbolic",
        "Save (Ctrl+S)",
        "win.save",
    ));
}

fn add_actions(app: &Rc<App>) {
    add_action(app, "open", |app| app.choose_file_to_open());
    add_action(app, "save", |app| app.save());
    add_action(app, "save-as", |app| app.save_as());
    add_action(app, "close", |app| app.ui.window.close());
    add_action(app, "undo", |app| app.step(OpenMode::Undo));
    add_action(app, "redo", |app| app.step(OpenMode::Redo));
    add_action(app, "rotate-left", |app| app.rotate(-90));
    add_action(app, "rotate-right", |app| app.rotate(90));
    add_action(app, "delete", |app| app.delete_pages());
    add_action(app, "select-all", |app| app.select_all());
    add_action(app, "zoom-in", |app| app.zoom_by(1));
    add_action(app, "zoom-out", |app| app.zoom_by(-1));
    add_action(app, "zoom-fit", |app| app.set_zoom(Zoom::FitWidth));
    add_action(app, "compress-screen", |app| {
        app.compress(CompressLevel::Screen)
    });
    add_action(app, "compress-print", |app| {
        app.compress(CompressLevel::Print)
    });
    add_action(app, "export-images", |app| app.export_images());
    add_action(app, "ocr", |app| app.run_ocr());
    add_action(app, "shortcuts", |app| app.show_shortcuts());
    add_action(app, "about", |app| app.show_about());
}

fn add_action(app: &Rc<App>, name: &'static str, handler: impl Fn(&Rc<App>) + 'static) {
    let action = gio::SimpleAction::new(name, None);
    action.connect_activate(clone!(
        #[strong]
        app,
        move |_, _| handler(&app)
    ));
    app.ui.window.add_action(&action);
    app.actions.borrow_mut().insert(name, action);
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
                    let path = paths.into_iter().next().unwrap();
                    app.confirm_discard(move |app| app.open(path));
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

/// Re-render on resize when fitting the width, and zoom on Ctrl+scroll.
fn add_view_controllers(app: &Rc<App>) {
    app.ui.viewer.hadjustment().connect_notify_local(
        Some("page-size"),
        clone!(
            #[weak]
            app,
            move |_, _| {
                if app.zoom.get() == Zoom::FitWidth && app.state.borrow().is_open() {
                    app.schedule_refit();
                }
            }
        ),
    );

    let scroll = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk4::PropagationPhase::Capture);
    scroll.connect_scroll(clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |controller, _, dy| {
            let ctrl = controller
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK);
            if !ctrl || dy == 0.0 || !app.state.borrow().is_open() {
                return glib::Propagation::Proceed;
            }
            app.zoom_by(if dy < 0.0 { 1 } else { -1 });
            glib::Propagation::Stop
        }
    ));
    app.ui.viewer.add_controller(scroll);
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
        if self.refuse_while_busy() {
            return;
        }
        self.open_mode.set(OpenMode::Fresh);
        self.start(&format!("Opening {}…", path.display()));
        let _ = self.to_worker.send(Request::Open {
            path,
            password: None,
        });
    }

    fn handle(self: &Rc<Self>, response: Response) {
        match response {
            Response::Opened { path, page_count } => {
                self.finish();
                let message = self.opened(path, page_count);
                self.generation.set(self.generation.get() + 1);
                self.after_edit(&message);
            }

            Response::Rendered {
                page,
                generation,
                purpose,
                dpi,
                image,
            } => {
                // A render for a document that has since been replaced.
                if generation != self.generation.get() {
                    return;
                }
                match purpose {
                    Purpose::Thumbnail => self.place_thumbnail(page, &image),
                    Purpose::View { ticket } => {
                        if ticket == self.view_ticket.get() {
                            self.place_view(page, dpi, &image);
                        }
                    }
                }
            }

            Response::Progress(message) => {
                self.ui.progress.set_visible(true);
                self.ui.progress.pulse();
                self.set_status(&message);
            }

            Response::SourceMoved { from, to } => {
                self.state.borrow_mut().relocate_source(&from, to);
            }

            Response::Saved { path, unprotected } => {
                self.finish();
                if let Some(written) = self.saving.take() {
                    self.state.borrow_mut().mark_saved(path.clone(), written);
                }
                self.refresh_title();
                if unprotected {
                    self.set_status(&format!(
                        "Saved {} without password protection.",
                        path.display()
                    ));
                } else {
                    self.set_status(&format!("Saved {}", path.display()));
                }

                let next = self.after_save.take();
                if let Some(next) = next {
                    next(self);
                }
            }

            Response::Finished { message } => {
                self.finish();
                self.set_status(&message);
            }

            Response::Failed { task, error } => {
                if task != Task::Render {
                    self.finish();
                }
                if task == Task::Save {
                    self.saving.replace(None);
                    self.after_save.replace(None);
                }
                if matches!(task, Task::Open | Task::Merge) {
                    match error {
                        PdfError::Encrypted(path) => {
                            return self.ask_to_unlock(task, path, false);
                        }
                        PdfError::WrongPassword(path) => {
                            return self.ask_to_unlock(task, path, true);
                        }
                        _ => {
                            self.open_mode.set(OpenMode::Fresh);
                            self.pending_merge.replace(None);
                        }
                    }
                }
                self.report(task, &error);
            }
        }
    }

    /// Bring the state up to date with a document the worker has opened, and
    /// say what happened.
    fn opened(self: &Rc<Self>, path: PathBuf, page_count: usize) -> String {
        let mode = self.open_mode.replace(OpenMode::Fresh);
        let mut state = self.state.borrow_mut();

        match mode {
            OpenMode::Fresh => {
                state.open(path, page_count);
                "Ready.".to_string()
            }
            OpenMode::Merge => {
                state.open_merged(path.clone(), page_count);
                format!(
                    "Merged into {}. Undo returns to the previous document.",
                    path.display()
                )
            }
            OpenMode::Undo | OpenMode::Redo => {
                let target = if mode == OpenMode::Undo {
                    state.peek_undo()
                } else {
                    state.peek_redo()
                };
                let matches = target.is_some_and(|snapshot| {
                    snapshot.source.as_deref() == Some(path.as_path())
                        && snapshot.page_count == page_count
                });

                if !matches {
                    // The file changed on disk since, so the old edits no
                    // longer describe it. Show it as it is now.
                    state.open(path, page_count);
                    return "That document has changed since, so its history was cleared."
                        .to_string();
                }

                if mode == OpenMode::Undo {
                    state.undo();
                    "Undone.".to_string()
                } else {
                    state.redo();
                    "Redone.".to_string()
                }
            }
        }
    }

    fn place_thumbnail(self: &Rc<Self>, page: usize, image: &pdf_core::render::RenderedPage) {
        let state = self.state.borrow();
        let pictures = self.thumbnail_pictures.borrow();

        for (position, source) in state.order.iter().enumerate() {
            if *source != page {
                continue;
            }
            if let Some(picture) = pictures.get(position) {
                picture.set_paintable(Some(&texture(image, state.rotation_at(position))));
            }
        }
    }

    fn place_view(self: &Rc<Self>, page: usize, dpi: f32, image: &pdf_core::render::RenderedPage) {
        let state = self.state.borrow();
        if state.source_page(state.current) != Some(page) {
            return;
        }

        let texture = texture(image, state.rotation_at(state.current));
        self.ui.page_view.set_paintable(Some(&texture));

        self.view_dpi.set(dpi);
        self.ui
            .zoom_level
            .set_label(&format!("{:.0}%", dpi / ZOOM_100_DPI * 100.0));
    }

    fn rebuild_thumbnails(self: &Rc<Self>) {
        while let Some(child) = self.ui.thumbnails.first_child() {
            self.ui.thumbnails.remove(&child);
        }
        self.thumbnail_pictures.borrow_mut().clear();

        let visible = self.state.borrow().visible_pages();
        let generation = self.generation.get();

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
                    size: Size::Box(THUMBNAIL_SIZE),
                    purpose: Purpose::Thumbnail,
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

    /// Ask for the current page at the current zoom level.
    fn show_current(self: &Rc<Self>) {
        let (page, quarter_turned) = {
            let state = self.state.borrow();
            (
                state.source_page(state.current),
                state.rotation_at(state.current) % 180 != 0,
            )
        };

        let Some(page) = page else {
            self.ui.page_view.set_paintable(None::<&gdk::Texture>);
            return;
        };

        let size = match self.zoom.get() {
            Zoom::Factor(factor) => Size::Dpi(ZOOM_100_DPI * factor),
            Zoom::FitWidth => {
                let available = self.ui.viewer.width() - 2 * VIEW_MARGIN;
                if available > 0 {
                    Size::Width {
                        pixels: available.max(64) as u32,
                        quarter_turned,
                    }
                } else {
                    // Not laid out yet; the resize that follows will refit.
                    Size::Dpi(ZOOM_100_DPI)
                }
            }
        };

        let ticket = self.view_ticket.get() + 1;
        self.view_ticket.set(ticket);

        let _ = self.to_worker.send(Request::Render {
            page,
            size,
            purpose: Purpose::View { ticket },
            generation: self.generation.get(),
        });
    }

    fn schedule_refit(self: &Rc<Self>) {
        if let Some(pending) = self.refit.take() {
            pending.remove();
        }
        let id = glib::timeout_add_local_once(
            Duration::from_millis(120),
            clone!(
                #[weak(rename_to = app)]
                self,
                move || {
                    app.refit.replace(None);
                    app.show_current();
                }
            ),
        );
        self.refit.replace(Some(id));
    }

    fn set_zoom(self: &Rc<Self>, zoom: Zoom) {
        self.zoom.set(zoom);
        self.show_current();
    }

    /// Step to the next zoom level up (positive) or down (negative) from
    /// whatever is showing now, including a fitted width.
    fn zoom_by(self: &Rc<Self>, direction: i32) {
        let now = self.view_dpi.get() / ZOOM_100_DPI;
        let next = if direction > 0 {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|step| *step > now * 1.01)
                .unwrap_or(ZOOM_STEPS[ZOOM_STEPS.len() - 1])
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|step| *step < now * 0.99)
                .unwrap_or(ZOOM_STEPS[0])
        };
        self.set_zoom(Zoom::Factor(next));
    }

    /// Redraw everything after the pages or the document changed.
    fn after_edit(self: &Rc<Self>, message: &str) {
        self.rebuild_thumbnails();
        self.show_current();
        self.refresh_title();
        self.refresh_actions();
        self.set_status(message);
    }

    fn rotate(self: &Rc<Self>, degrees: i32) {
        if !self.require_document() {
            return;
        }

        self.state.borrow_mut().rotate(degrees);
        self.after_edit("Rotated. Save to write the change out.");
    }

    fn delete_pages(self: &Rc<Self>) {
        if !self.require_document() {
            return;
        }

        let outcome = self.state.borrow_mut().delete();
        match outcome {
            Ok(()) => self.after_edit("Deleted. Save to write the change out."),
            Err(reason) => self.set_status(&format!("Cannot delete: {reason}")),
        }
    }

    fn select_all(self: &Rc<Self>) {
        if !self.require_document() {
            return;
        }
        self.state.borrow_mut().select_all();
        self.refresh_selection();
        let count = self.state.borrow().selection.len();
        self.set_status(&format!("Selected all {count} pages."));
    }

    fn select(self: &Rc<Self>, position: usize, extend: bool) {
        let moved = {
            let mut state = self.state.borrow_mut();
            let before = state.current;
            if extend {
                state.toggle_selection(position);
            } else {
                state.select_only(position);
            }
            state.current != before
        };

        self.refresh_selection();
        if moved {
            self.ui.viewer.vadjustment().set_value(0.0);
            self.show_current();
        }
    }

    fn move_page(self: &Rc<Self>, from: usize, to: usize) {
        if self.refuse_while_busy() {
            return;
        }
        self.state.borrow_mut().move_page(from, to);
        self.after_edit("Reordered. Save to write the change out.");
    }

    /// Undo or redo one step. Stepping to a different document, which happens
    /// only across a merge, means reopening it on the worker first.
    fn step(self: &Rc<Self>, direction: OpenMode) {
        if self.refuse_while_busy() {
            return;
        }

        let reopen = {
            let state = self.state.borrow();
            let target = if direction == OpenMode::Undo {
                state.peek_undo()
            } else {
                state.peek_redo()
            };
            match target {
                None => return,
                Some(snapshot) if snapshot.source != state.source => snapshot.source.clone(),
                Some(_) => None,
            }
        };

        match reopen {
            Some(source) => {
                self.open_mode.set(direction);
                self.start("Reopening the previous document…");
                let _ = self.to_worker.send(Request::Open {
                    path: source,
                    password: None,
                });
            }
            None => {
                let message = {
                    let mut state = self.state.borrow_mut();
                    if direction == OpenMode::Undo {
                        state.undo();
                        "Undone."
                    } else {
                        state.redo();
                        "Redone."
                    }
                };
                self.after_edit(message);
            }
        }
    }

    fn merge_dropped(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if self.refuse_while_busy() {
            return;
        }

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
                    let Some(output) = file.path() else {
                        return;
                    };
                    if app.refuse_while_busy() {
                        return;
                    }

                    app.start(&format!("Merging {} files…", paths.len()));
                    app.merge(paths, output, None);
                }
            ),
        );
    }

    /// Send a merge to the worker, remembering it in case an input turns out
    /// to need a password and the merge has to be tried again.
    fn merge(
        self: &Rc<Self>,
        inputs: Vec<PathBuf>,
        output: PathBuf,
        unlock: Option<(PathBuf, String)>,
    ) {
        self.open_mode.set(OpenMode::Merge);
        self.pending_merge
            .replace(Some((inputs.clone(), output.clone())));
        let _ = self.to_worker.send(Request::Merge {
            inputs,
            output,
            unlock,
        });
    }

    /// An open or a merge stopped at a file that needs a password. Ask for
    /// it, then try again; `wrong` says the last password given did not work.
    fn ask_to_unlock(self: &Rc<Self>, task: Task, path: PathBuf, wrong: bool) {
        let merge = if task == Task::Merge {
            match self.pending_merge.take() {
                Some(merge) => Some(merge),
                None => return,
            }
        } else {
            None
        };

        let locked = path.clone();
        password_dialog(
            &self.ui.window,
            &file_name(&path),
            wrong,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |answer| {
                    let Some(password) = answer else {
                        app.open_mode.set(OpenMode::Fresh);
                        app.set_status("Not opened: it needs a password.");
                        return;
                    };
                    if app.refuse_while_busy() {
                        return;
                    }

                    app.start("Unlocking…");
                    match merge {
                        Some((inputs, output)) => {
                            app.merge(inputs, output, Some((locked, password)))
                        }
                        None => {
                            // The open mode is still that of the attempt that
                            // asked for the password.
                            let _ = app.to_worker.send(Request::Open {
                                path: locked,
                                password: Some(password),
                            });
                        }
                    }
                }
            ),
        );
    }

    fn choose_file_to_open(self: &Rc<Self>) {
        self.confirm_discard(|app| app.show_open_dialog());
    }

    fn show_open_dialog(self: &Rc<Self>) {
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

    /// Write the edits over the document's own file.
    fn save(self: &Rc<Self>) {
        if !self.require_document() {
            return;
        }
        if !self.state.borrow().is_dirty() {
            self.set_status("No changes to save.");
            return;
        }
        let path = self.state.borrow().path.clone();
        if let Some(path) = path {
            self.write_to(path);
        }
    }

    fn save_as(self: &Rc<Self>) {
        if !self.require_document() {
            return;
        }

        let name = self.state.borrow().suggested_name("-edited", "pdf");
        self.ask_for_file(&name, |app, destination| app.write_to(destination));
    }

    fn write_to(self: &Rc<Self>, destination: PathBuf) {
        if self.refuse_while_busy() {
            return;
        }
        let edits = self.state.borrow().edits();
        self.saving.replace(Some(edits.clone()));
        self.start("Saving…");
        let _ = self.to_worker.send(Request::Save {
            path: destination,
            edits,
        });
    }

    /// Run `then` straight away if there is nothing unsaved; otherwise ask
    /// whether to save, discard, or stay put first.
    fn confirm_discard(self: &Rc<Self>, then: impl FnOnce(&Rc<Self>) + 'static) {
        let name = {
            let state = self.state.borrow();
            if !state.is_dirty() {
                drop(state);
                then(self);
                return;
            }
            state
                .display_name()
                .unwrap_or_else(|| "the document".to_string())
        };

        let dialog = AlertDialog::builder()
            .modal(true)
            .message(format!("Save changes to “{name}”?"))
            .detail("If you don’t save, your changes to its pages will be lost.")
            .buttons(["Cancel", "Discard", "Save"])
            .cancel_button(0)
            .default_button(2)
            .build();

        dialog.choose(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |choice| match choice {
                    Ok(1) => then(&app),
                    Ok(2) => {
                        let path = app.state.borrow().path.clone();
                        if let Some(path) = path {
                            app.after_save.replace(Some(Box::new(then)));
                            app.write_to(path);
                        }
                    }
                    _ => {}
                }
            ),
        );
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
            self.alert(
                "Text recognition is not available",
                "Foolscap uses Tesseract to recognise text, and it is not installed.\n\n\
                 Install it with: sudo apt install tesseract-ocr",
            );
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
        let mut builder = FileDialog::builder()
            .title("Save as")
            .initial_name(suggested);

        // Start in the document's own folder rather than wherever the file
        // chooser was last.
        let folder = self
            .state
            .borrow()
            .path
            .as_ref()
            .and_then(|path| path.parent())
            .map(gio::File::for_path);
        if let Some(folder) = folder {
            builder = builder.initial_folder(&folder);
        }

        builder.build().save(
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

    fn show_shortcuts(self: &Rc<Self>) {
        let builder = gtk4::Builder::from_string(SHORTCUTS_UI);
        let Some(window) = builder.object::<gtk4::ShortcutsWindow>("shortcuts") else {
            return;
        };
        window.set_transient_for(Some(&self.ui.window));
        window.present();
    }

    fn show_about(self: &Rc<Self>) {
        let authors: Vec<String> = env!("CARGO_PKG_AUTHORS")
            .split(':')
            .map(str::to_string)
            .collect();

        let about = gtk4::AboutDialog::builder()
            .transient_for(&self.ui.window)
            .modal(true)
            .program_name("Foolscap")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Rearrange, compress, convert and recognise text in PDF documents.")
            .website("https://github.com/Sarcastic-Soul/Foolscap")
            .website_label("Source code")
            .license_type(gtk4::License::Agpl30)
            .authors(authors)
            .build();
        if let Some(icon) = installed_icon() {
            about.set_logo_icon_name(Some(icon));
        }
        about.present();
    }

    /// Mark the start of a long operation. Only one runs at a time: the worker
    /// is a single thread, and queueing them silently would look like a hang.
    fn start(self: &Rc<Self>, message: &str) {
        self.busy.set(true);
        self.ui.progress.set_visible(true);
        self.ui.progress.pulse();
        self.set_status(message);
        self.refresh_actions();
    }

    fn finish(self: &Rc<Self>) {
        self.busy.set(false);
        self.ui.progress.set_visible(false);
        self.refresh_actions();
    }

    fn refuse_while_busy(self: &Rc<Self>) -> bool {
        if self.busy.get() {
            self.set_status("Still working on the last request…");
        }
        self.busy.get()
    }

    fn require_document(self: &Rc<Self>) -> bool {
        if self.refuse_while_busy() {
            return false;
        }

        if !self.state.borrow().is_open() {
            self.set_status("Open a PDF first.");
            return false;
        }

        true
    }

    /// Grey out whatever cannot be done right now.
    fn refresh_actions(self: &Rc<Self>) {
        let state = self.state.borrow();
        let idle = !self.busy.get();
        let open = state.is_open();
        let actions = self.actions.borrow();

        let enable = |name: &str, enabled: bool| {
            if let Some(action) = actions.get(name) {
                action.set_enabled(enabled);
            }
        };

        for name in DOCUMENT_ACTIONS {
            enable(name, idle && open);
        }
        enable("open", idle);
        enable("save", idle && state.is_dirty());
        enable("undo", idle && state.can_undo());
        enable("redo", idle && state.can_redo());
        for name in ["zoom-in", "zoom-out", "zoom-fit"] {
            enable(name, open);
        }
    }

    fn refresh_title(self: &Rc<Self>) {
        let state = self.state.borrow();
        let Some(name) = state.display_name() else {
            self.ui.subtitle.set_label("No document");
            self.ui.window.set_title(Some("Foolscap"));
            return;
        };

        let edited = if state.is_dirty() { " (edited)" } else { "" };
        let pages = state.visible_pages();
        self.ui.subtitle.set_label(&format!(
            "{name}{edited} — {pages} page{}",
            if pages == 1 { "" } else { "s" }
        ));
        self.ui
            .window
            .set_title(Some(&format!("{name}{edited} — Foolscap")));
    }

    /// Tell the user something went wrong: a dialog for anything they asked
    /// for, the status bar for a page that failed to draw.
    fn report(self: &Rc<Self>, task: Task, error: &PdfError) {
        tracing::warn!(?task, %error, "operation failed");
        self.set_status(&format!("Failed: {error}"));

        if task != Task::Render {
            let (heading, detail) = explain(task, error);
            self.alert(&heading, &detail);
        }
    }

    fn alert(self: &Rc<Self>, heading: &str, detail: &str) {
        AlertDialog::builder()
            .modal(true)
            .message(heading)
            .detail(detail)
            .build()
            .show(Some(&self.ui.window));
    }

    fn set_status(&self, message: &str) {
        self.ui.status.set_label(message);
    }
}

/// The last part of a path, as a user would recognise the file.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Ask for the password to a protected file. `done` receives the password, or
/// `None` if the user gave up. `wrong` shows that the last attempt failed.
fn password_dialog(
    parent: &ApplicationWindow,
    name: &str,
    wrong: bool,
    done: impl FnOnce(Option<String>) + 'static,
) {
    let dialog = gtk4::Window::builder()
        .title("Unlock Document")
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .destroy_with_parent(true)
        .default_width(380)
        .build();

    let content = GtkBox::new(Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let heading = Label::builder()
        .label(format!("“{name}” is password protected"))
        .wrap(true)
        .max_width_chars(40)
        .xalign(0.0)
        .css_classes(["title-4"])
        .build();
    let detail = Label::builder()
        .label("Enter its password to open it.")
        .wrap(true)
        .max_width_chars(40)
        .xalign(0.0)
        .build();

    let entry = gtk4::PasswordEntry::builder()
        .show_peek_icon(true)
        .activates_default(true)
        .hexpand(true)
        .build();
    entry.update_property(&[gtk4::accessible::Property::Label("Password")]);

    let problem = Label::builder()
        .label("Wrong password. Try again.")
        .xalign(0.0)
        .css_classes(["error"])
        .visible(wrong)
        .build();

    let cancel = Button::with_label("Cancel");
    let unlock = Button::builder()
        .label("Unlock")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.set_halign(Align::End);
    buttons.set_margin_top(6);
    buttons.append(&cancel);
    buttons.append(&unlock);

    content.append(&heading);
    content.append(&detail);
    content.append(&entry);
    content.append(&problem);
    content.append(&buttons);
    dialog.set_child(Some(&content));
    dialog.set_default_widget(Some(&unlock));

    // The answer is given once: by Unlock, or by any way of closing the
    // dialog, which counts as Cancel.
    type Answer = Box<dyn FnOnce(Option<String>)>;
    let done: Rc<RefCell<Option<Answer>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    let answer = {
        let done = done.clone();
        let dialog = dialog.clone();
        move |password: Option<String>| {
            let callback = done.borrow_mut().take();
            dialog.close();
            if let Some(callback) = callback {
                callback(password);
            }
        }
    };
    let answer = Rc::new(answer);

    entry.connect_changed(clone!(
        #[weak]
        unlock,
        #[weak]
        problem,
        move |entry| {
            unlock.set_sensitive(!entry.text().is_empty());
            problem.set_visible(false);
        }
    ));
    unlock.connect_clicked(clone!(
        #[weak]
        entry,
        #[strong]
        answer,
        move |_| {
            if !entry.text().is_empty() {
                answer(Some(entry.text().to_string()));
            }
        }
    ));
    cancel.connect_clicked(clone!(
        #[strong]
        answer,
        move |_| answer(None)
    ));
    dialog.connect_close_request(clone!(
        #[strong]
        done,
        move |_| {
            if let Some(callback) = done.borrow_mut().take() {
                callback(None);
            }
            glib::Propagation::Proceed
        }
    ));

    let escape = gtk4::EventControllerKey::new();
    escape.connect_key_pressed(clone!(
        #[strong]
        answer,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                answer(None);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    dialog.add_controller(escape);

    dialog.present();
    entry.grab_focus();
}

/// A heading and an explanation for a failure, in words a user can act on.
fn explain(task: Task, error: &PdfError) -> (String, String) {
    let heading = match task {
        Task::Open => "Could not open the document",
        Task::Render => "Could not draw a page",
        Task::Save => "Could not save the document",
        Task::Compress => "Could not compress the document",
        Task::Ocr => "Could not make the document searchable",
        Task::Export => "Could not export the pages",
        Task::Merge => "Could not merge the files",
    }
    .to_string();

    let name = file_name;

    let detail = match error {
        PdfError::Encrypted(path) => format!(
            "“{}” is password protected. Open it again and enter its password.",
            name(path)
        ),
        PdfError::WrongPassword(path) => {
            format!("The password given for “{}” is not correct.", name(path))
        }
        PdfError::UnsupportedEncryption { path, .. } => format!(
            "“{}” is protected in a way Foolscap cannot read, such as with a certificate \
             rather than a password.",
            name(path)
        ),
        PdfError::NotFound(path) => format!(
            "“{}” could not be found. It may have been moved or deleted.",
            name(path)
        ),
        PdfError::Malformed { path, reason } | PdfError::Render { path, reason }
            if matches!(task, Task::Open | Task::Merge) =>
        {
            format!("“{}” is damaged or is not a PDF.\n\n{reason}", name(path))
        }
        PdfError::Io { path, source } if source.kind() == std::io::ErrorKind::PermissionDenied => {
            format!(
                "You do not have permission to use {}. Try saving somewhere else.",
                path.display()
            )
        }
        PdfError::ToolMissing { tool, install } => {
            format!("{tool} is not installed.\n\nInstall it with: {install}")
        }
        other => {
            let text = other.to_string();
            let mut chars = text.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => text,
            }
        }
    };

    (heading, detail)
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

/// The keyboard shortcuts window, matching `ACCELS`.
const SHORTCUTS_UI: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<interface>
  <object class="GtkShortcutsWindow" id="shortcuts">
    <property name="modal">true</property>
    <child>
      <object class="GtkShortcutsSection">
        <property name="section-name">main</property>
        <property name="max-height">16</property>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Document</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Open</property>
                <property name="accelerator">&lt;Primary&gt;o</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Save</property>
                <property name="accelerator">&lt;Primary&gt;s</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Save as</property>
                <property name="accelerator">&lt;Primary&gt;&lt;Shift&gt;s</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Close the window</property>
                <property name="accelerator">&lt;Primary&gt;w</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Quit</property>
                <property name="accelerator">&lt;Primary&gt;q</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Pages</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Undo</property>
                <property name="accelerator">&lt;Primary&gt;z</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Redo</property>
                <property name="accelerator">&lt;Primary&gt;&lt;Shift&gt;z &lt;Primary&gt;y</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Select all pages</property>
                <property name="accelerator">&lt;Primary&gt;a</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Select more pages</property>
                <property name="accelerator">&lt;Primary&gt;</property>
                <property name="subtitle">Hold while clicking thumbnails</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Delete the selected pages</property>
                <property name="accelerator">Delete</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Rotate anticlockwise</property>
                <property name="accelerator">&lt;Primary&gt;bracketleft &lt;Primary&gt;l</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Rotate clockwise</property>
                <property name="accelerator">&lt;Primary&gt;bracketright &lt;Primary&gt;r</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">View</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Zoom in</property>
                <property name="accelerator">&lt;Primary&gt;plus</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Zoom out</property>
                <property name="accelerator">&lt;Primary&gt;minus</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Fit the page to the window width</property>
                <property name="accelerator">&lt;Primary&gt;0</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Keyboard shortcuts</property>
                <property name="accelerator">&lt;Primary&gt;question</property>
              </object>
            </child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_encrypted_file_is_explained_plainly() {
        let (heading, detail) = explain(
            Task::Open,
            &PdfError::Encrypted(PathBuf::from("/home/me/secret.pdf")),
        );
        assert_eq!(heading, "Could not open the document");
        assert!(detail.contains("“secret.pdf” is password protected"));
        assert!(!detail.contains("cannot open"));
    }

    #[test]
    fn a_wrong_password_names_the_file() {
        let (_, detail) = explain(
            Task::Merge,
            &PdfError::WrongPassword(PathBuf::from("/home/me/secret.pdf")),
        );
        assert!(detail.contains("“secret.pdf” is not correct"));
    }

    #[test]
    fn a_missing_file_names_the_file() {
        let (_, detail) = explain(Task::Open, &PdfError::NotFound(PathBuf::from("/a/b.pdf")));
        assert!(detail.contains("“b.pdf” could not be found"));
    }

    #[test]
    fn other_errors_read_as_a_sentence() {
        let (heading, detail) = explain(Task::Save, &PdfError::EmptySelection);
        assert_eq!(heading, "Could not save the document");
        assert_eq!(detail, "No pages selected");
    }

    #[test]
    fn every_shortcut_names_a_known_action() {
        let window_actions = [
            "open",
            "save",
            "save-as",
            "close",
            "undo",
            "redo",
            "select-all",
            "delete",
            "rotate-left",
            "rotate-right",
            "zoom-in",
            "zoom-out",
            "zoom-fit",
            "shortcuts",
        ];
        for (action, accels) in ACCELS {
            assert!(!accels.is_empty());
            match action.split_once('.') {
                Some(("win", name)) => assert!(window_actions.contains(&name), "{action}"),
                Some(("app", "quit")) => {}
                _ => panic!("unexpected action {action}"),
            }
        }
    }
}
