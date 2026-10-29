//! The main window.
//!
//! The layout is a thumbnail sidebar beside a page view, with the actions on a
//! header bar. Every slow call goes to the worker thread and comes back as a
//! message, so the interface stays responsive while a document is being
//! recognised or compressed.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use gtk4::gdk;
use gtk4::gio;
use gtk4::glib::{self, clone};
use gtk4::prelude::*;
use gtk4::{
    AlertDialog, Align, Application, ApplicationWindow, Box as GtkBox, Button, DropDown,
    DropTarget, FileDialog, FlowBox, HeaderBar, Label, ListBox, MenuButton, Orientation, Paned,
    Picture, PopoverMenu, ProgressBar, ScrolledWindow, SelectionMode, Separator, Stack,
    ToggleButton,
};
use pdf_core::convert::{ImageFormat, OfficeFormat};
use pdf_core::ops::FlattenOptions;
use pdf_core::render::RenderedPage;
use pdf_core::{
    Anchor, CompressLevel, Fit, Margins, PageRange, PageSize, PdfError, Point, Rect, StampContent,
    StampFont, StampLayer, StampOptions, Stroke,
};

use crate::image::{diff, texture};
use crate::queue::{JobStatus, Queue};
use crate::state::{ComparisonSide, ComparisonState, State};
use crate::tools::{Tool, TOOLS};
use crate::units::{check_margins, kept_fractions, MarginValues, Unit};
use crate::worker::{
    self, CompareRenderRequest, CropAction, Edits, ImageOutput, Purpose, RedactTarget, RenderSlot,
    Request, Response, Size, Task,
};

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
    ("win.dashboard", &["<Primary>d"]),
    ("win.compare-open-left", &["<Primary><Alt>1"]),
    ("win.compare-open-right", &["<Primary><Alt>2"]),
    ("win.compare-toggle-sync", &["<Primary><Alt>s"]),
    ("win.compare-toggle-diff", &["<Primary><Alt>d"]),
    ("win.compare-focus-left", &["<Primary>1"]),
    ("win.compare-focus-right", &["<Primary>2"]),
    ("win.cancel-waiting", &["<Primary><Shift>Escape"]),
];

/// Actions that need an open document and no running job: they act on the
/// window state immediately rather than queueing.
const DOCUMENT_ACTIONS: &[&str] = &[
    "save-as",
    "rotate-left",
    "rotate-right",
    "delete",
    "select-all",
];

/// Compare actions that only make sense with both sides open.
const COMPARE_PAIR_ACTIONS: &[&str] = &[
    "compare-toggle-sync",
    "compare-toggle-diff",
    "compare-focus-left",
    "compare-focus-right",
];

/// Opening a file into the compare view only needs an idle worker.
const COMPARE_OPEN_ACTIONS: &[&str] = &["compare-open-left", "compare-open-right"];

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
pub enum Zoom {
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

/// One side's controls in the compare view. The widgets are held here rather
/// than found again by walking the tree.
#[derive(Clone)]
struct SideBar {
    root: GtkBox,
    title: Label,
    pages: Label,
    prev: Button,
    next: Button,
    zoom_out: Button,
    zoom_in: Button,
    fit: Button,
}

impl SideBar {
    fn new(heading: &str) -> Self {
        let small = |icon: &str, tip: &str| {
            let button = Button::from_icon_name(icon);
            button.set_tooltip_text(Some(tip));
            button.add_css_class("flat");
            button
        };

        let title = Label::builder()
            .label(heading)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk4::pango::EllipsizeMode::Middle)
            .css_classes(vec!["heading".to_string()])
            .build();
        let pages = Label::builder()
            .label("–")
            .width_chars(8)
            .css_classes(vec!["dim-label".to_string()])
            .build();
        let prev = small("go-previous-symbolic", "Previous page");
        let next = small("go-next-symbolic", "Next page");
        let zoom_out = small("zoom-out-symbolic", "Zoom this side out");
        let zoom_in = small("zoom-in-symbolic", "Zoom this side in");
        let fit = small("zoom-fit-best-symbolic", "Fit this side to its width");

        let root = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(2)
            .margin_start(8)
            .margin_end(8)
            .margin_top(2)
            .margin_bottom(2)
            .build();
        root.append(&title);
        root.append(&prev);
        root.append(&pages);
        root.append(&next);
        root.append(&zoom_out);
        root.append(&zoom_in);
        root.append(&fit);

        Self {
            root,
            title,
            pages,
            prev,
            next,
            zoom_out,
            zoom_in,
            fit,
        }
    }
}

struct Ui {
    window: ApplicationWindow,
    stack: Stack,
    thumbnails: FlowBox,
    viewer: ScrolledWindow,
    page_view: Picture,
    status: Label,
    progress: ProgressBar,
    subtitle: Label,
    zoom_level: Label,
    /// One button per dashboard card, so tooltips can follow what is possible.
    tool_buttons: Vec<(&'static Tool, Button)>,
    queue_list: ListBox,
    queue_label: Label,
    queue_scroll: ScrolledWindow,
    queue_cancel: Button,
    queue_clear: Button,
    /// Compare view: left side viewer.
    compare_left_viewer: ScrolledWindow,
    compare_left_page: Picture,
    compare_left_bar: SideBar,
    /// Compare view: right side viewer.
    compare_right_viewer: ScrolledWindow,
    compare_right_page: Picture,
    compare_right_bar: SideBar,
    /// Compare view: sync toggle button.
    compare_sync_button: ToggleButton,
    /// Compare view: diff toggle button.
    compare_diff_button: ToggleButton,
}

/// The latest render of the left and the right compare viewer.
type ComparePair = (Option<Arc<RenderedPage>>, Option<Arc<RenderedPage>>);

struct App {
    ui: Ui,
    state: RefCell<State>,
    /// State for the compare view (two documents side by side).
    compare_state: RefCell<ComparisonState>,
    to_worker: mpsc::Sender<Request>,
    /// Incremented whenever the document changes, so that renders requested for
    /// an older document can be recognised and thrown away when they arrive.
    generation: Cell<u64>,
    /// The batch job on the worker now, if any. Immediate operations (open,
    /// save) hold a sentinel while they run. Replaces the old `busy` flag:
    /// `is_some` means busy, and the queue panel shows what is waiting behind.
    busy_job: Cell<Option<u64>>,
    /// Batch requests waiting for the worker, oldest first. The worker itself
    /// is a single thread, so sending them one at a time keeps MuPDF and
    /// LibreOffice serial.
    pending: RefCell<VecDeque<Request>>,
    /// Labels and progress for the queue panel. No IO happens here.
    queue: RefCell<Queue>,
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
    /// Compare view: separate generation counter for the secondary document.
    compare_generation: Cell<u64>,
    /// Compare view: render ticket for the left viewer.
    compare_left_ticket: Cell<u64>,
    /// Compare view: render ticket for the right viewer.
    compare_right_ticket: Cell<u64>,
    /// Compare view: DPI for the left viewer.
    compare_left_dpi: Cell<f32>,
    /// Compare view: DPI for the right viewer.
    compare_right_dpi: Cell<f32>,
    /// Compare view: zoom level for the left viewer.
    compare_left_zoom: Cell<Zoom>,
    /// Compare view: zoom level for the right viewer.
    compare_right_zoom: Cell<Zoom>,
    /// Compare view: whether diff mode is enabled.
    compare_diff: Cell<bool>,
    /// Compare view: the side an open is in flight for, kept so that asking
    /// for a password can try the same side again.
    compare_pending: Cell<Option<ComparisonSide>>,
    /// Compare view: the latest render of each side, kept so the diff overlay
    /// can be drawn once both have arrived.
    compare_images: RefCell<ComparePair>,
    /// A pending re-render of the compare view after a resize.
    compare_refit: RefCell<Option<glib::SourceId>>,
    /// The view the dashboard is covering, so leaving it goes back there.
    previous_view: Cell<&'static str>,
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
    let page_view = page_picture();
    let viewer = ScrolledWindow::builder().child(&page_view).build();

    let paned = Paned::builder()
        .orientation(Orientation::Horizontal)
        .start_child(&sidebar)
        .end_child(&viewer)
        .resize_start_child(false)
        .shrink_start_child(false)
        .build();

    // Compare view: two viewers side by side, each under its own controls.
    let compare_left_page = page_picture();
    let compare_left_viewer = ScrolledWindow::builder()
        .child(&compare_left_page)
        .hexpand(true)
        .vexpand(true)
        .build();
    let compare_left_bar = SideBar::new("Left");
    let compare_left_side = GtkBox::new(Orientation::Vertical, 0);
    compare_left_side.append(&compare_left_bar.root);
    compare_left_side.append(&compare_left_viewer);

    let compare_right_page = page_picture();
    let compare_right_viewer = ScrolledWindow::builder()
        .child(&compare_right_page)
        .hexpand(true)
        .vexpand(true)
        .build();
    let compare_right_bar = SideBar::new("Right");
    let compare_right_side = GtkBox::new(Orientation::Vertical, 0);
    compare_right_side.append(&compare_right_bar.root);
    compare_right_side.append(&compare_right_viewer);

    let compare_paned = Paned::builder()
        .orientation(Orientation::Horizontal)
        .start_child(&compare_left_side)
        .end_child(&compare_right_side)
        .resize_start_child(true)
        .resize_end_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .wide_handle(true)
        .build();

    let compare_sync_button = ToggleButton::builder()
        .label("Sync")
        .tooltip_text("Scroll, zoom and change pages together (Ctrl+Alt+S)")
        .build();

    let compare_diff_button = ToggleButton::builder()
        .label("Show differences")
        .tooltip_text("Mark the pixels that differ between the two pages (Ctrl+Alt+D)")
        .build();

    let compare_header = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(4)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(8)
        .margin_end(8)
        .build();
    compare_header.append(
        &Label::builder()
            .label("Compare")
            .css_classes(vec!["title-4".to_string()])
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    let open_left = Button::with_label("Open left…");
    open_left.set_action_name(Some("win.compare-open-left"));
    let open_right = Button::with_label("Open right…");
    open_right.set_action_name(Some("win.compare-open-right"));
    compare_header.append(&open_left);
    compare_header.append(&open_right);
    compare_header.append(&compare_sync_button);
    compare_header.append(&compare_diff_button);

    let compare_box = GtkBox::new(Orientation::Vertical, 0);
    compare_box.append(&compare_header);
    compare_box.append(&compare_paned);
    compare_box.set_vexpand(true);

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

    let queue_label = Label::builder()
        .label("Queue")
        .xalign(0.0)
        .hexpand(true)
        .margin_start(8)
        .css_classes(vec!["dim-label".to_string()])
        .build();
    let queue_cancel = Button::builder()
        .label("Cancel waiting")
        .tooltip_text("Drop the jobs that have not started (Ctrl+Shift+Esc)")
        .css_classes(vec!["flat".to_string()])
        .action_name("win.cancel-waiting")
        .build();
    let queue_clear = Button::builder()
        .label("Clear finished")
        .tooltip_text("Remove finished and failed jobs from the list")
        .css_classes(vec!["flat".to_string()])
        .build();
    let queue_header = GtkBox::new(Orientation::Horizontal, 4);
    queue_header.append(&queue_label);
    queue_header.append(&queue_cancel);
    queue_header.append(&queue_clear);
    let queue_list = ListBox::builder()
        .selection_mode(SelectionMode::None)
        .build();
    let queue_scroll = ScrolledWindow::builder()
        .child(&queue_list)
        .max_content_height(120)
        .propagate_natural_height(true)
        .visible(false)
        .build();
    let queue_box = GtkBox::new(Orientation::Vertical, 0);
    queue_box.append(&queue_header);
    queue_box.append(&queue_scroll);

    // The tool panel: the dashboard grid on one side, the document view on
    // the other. Cards start jobs; the document view shows the open file.
    let stack = Stack::builder().build();
    let (dashboard, tool_buttons) = build_dashboard();
    let dashboard_scroll = ScrolledWindow::builder()
        .child(&dashboard)
        .vexpand(true)
        .build();
    stack.add_named(&dashboard_scroll, Some("dashboard"));
    stack.add_named(&paned, Some("document"));
    stack.add_named(&compare_box, Some("compare"));

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.append(&stack);
    content.append(&Separator::new(Orientation::Horizontal));
    content.append(&queue_box);
    content.append(&Separator::new(Orientation::Horizontal));
    content.append(&status_bar);
    paned.set_vexpand(true);
    compare_paned.set_vexpand(true);

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
            stack: stack.clone(),
            thumbnails: thumbnails.clone(),
            viewer: viewer.clone(),
            page_view: page_view.clone(),
            status: status.clone(),
            progress: progress.clone(),
            subtitle: subtitle.clone(),
            zoom_level: zoom_level.clone(),
            tool_buttons,
            queue_list: queue_list.clone(),
            queue_label: queue_label.clone(),
            queue_scroll: queue_scroll.clone(),
            queue_cancel: queue_cancel.clone(),
            queue_clear: queue_clear.clone(),
            compare_left_viewer: compare_left_viewer.clone(),
            compare_left_page: compare_left_page.clone(),
            compare_left_bar,
            compare_right_viewer: compare_right_viewer.clone(),
            compare_right_page: compare_right_page.clone(),
            compare_right_bar,
            compare_sync_button: compare_sync_button.clone(),
            compare_diff_button: compare_diff_button.clone(),
        },
        state: RefCell::new(State::default()),
        compare_state: RefCell::new(ComparisonState::default()),
        to_worker,
        generation: Cell::new(0),
        busy_job: Cell::new(None),
        pending: RefCell::new(VecDeque::new()),
        queue: RefCell::new(Queue::new()),
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
        compare_generation: Cell::new(0),
        compare_left_ticket: Cell::new(0),
        compare_right_ticket: Cell::new(0),
        compare_left_dpi: Cell::new(ZOOM_100_DPI),
        compare_right_dpi: Cell::new(ZOOM_100_DPI),
        compare_left_zoom: Cell::new(Zoom::FitWidth),
        compare_right_zoom: Cell::new(Zoom::FitWidth),
        compare_diff: Cell::new(false),
        compare_pending: Cell::new(None),
        compare_images: RefCell::new((None, None)),
        compare_refit: RefCell::new(None),
        previous_view: Cell::new("document"),
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

    queue_clear.connect_clicked(clone!(
        #[weak]
        app_state,
        move |_| {
            app_state.queue.borrow_mut().clear_finished();
            app_state.refresh_queue();
        }
    ));

    add_actions(&app_state);
    add_header_buttons(&header);
    add_drop_target(&window, &app_state);
    add_view_controllers(&app_state);
    add_compare_controllers(&app_state);
    listen(&app_state, from_worker);
    app_state.refresh_title();
    app_state.refresh_actions();
    app_state.refresh_queue();
    app_state.show_dashboard();

    tracing::info!("presenting the window");
    window.present();

    if let Some(path) = open {
        app_state.open(path);
    }
}

/// A picture that shows a page at the size it was rendered.
fn page_picture() -> Picture {
    Picture::builder()
        .can_shrink(false)
        .halign(Align::Center)
        .valign(Align::Center)
        .margin_top(VIEW_MARGIN)
        .margin_bottom(VIEW_MARGIN)
        .margin_start(VIEW_MARGIN)
        .margin_end(VIEW_MARGIN)
        .build()
}

/// One dashboard grid for every tool in the registry. Each card is a button
/// bound to its window action, so `refresh_actions` greys it out with no
/// extra wiring when a document or a helper program is missing. The buttons
/// are returned so their tooltips can follow what is possible.
fn build_dashboard() -> (FlowBox, Vec<(&'static Tool, Button)>) {
    let grid = FlowBox::builder()
        .selection_mode(SelectionMode::None)
        .min_children_per_line(2)
        .max_children_per_line(6)
        .row_spacing(12)
        .column_spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .halign(Align::Center)
        .valign(Align::Start)
        .build();

    let mut buttons = Vec::with_capacity(TOOLS.len());
    for tool in TOOLS {
        let icon = gtk4::Image::from_icon_name(tool.icon);
        icon.set_pixel_size(40);
        let title = Label::builder()
            .label(tool.title)
            .css_classes(vec!["title-4".to_string()])
            .build();
        let subtitle = Label::builder()
            .label(tool.subtitle)
            .css_classes(vec!["dim-label".to_string()])
            .wrap(true)
            .justify(gtk4::Justification::Center)
            .max_width_chars(18)
            .build();
        let card = GtkBox::new(Orientation::Vertical, 4);
        card.set_margin_top(12);
        card.set_margin_bottom(12);
        card.set_margin_start(8);
        card.set_margin_end(8);
        card.append(&icon);
        card.append(&title);
        card.append(&subtitle);

        let button = Button::builder()
            .child(&card)
            .width_request(150)
            .height_request(110)
            .build();
        button.set_action_name(Some(&format!("win.{}", tool.id.action())));
        button.set_tooltip_text(Some(&card_tooltip(tool, false)));
        grid.insert(&button, -1);
        buttons.push((tool, button));
    }

    (grid, buttons)
}

/// What a card's tooltip says: what the tool does and, when it cannot run
/// right now, why not.
fn card_tooltip(tool: &Tool, doc_open: bool) -> String {
    match tool.disabled_reason(doc_open) {
        Some(reason) => format!("{} — {reason}", tool.subtitle),
        None => tool.subtitle.to_string(),
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
        "view-grid-symbolic",
        "Show the tool dashboard (Ctrl+D)",
        "win.dashboard",
    ));
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

    let compare = gio::Menu::new();
    compare.append(Some("Compare: open left…"), Some("win.compare-open-left"));
    compare.append(Some("Compare: open right…"), Some("win.compare-open-right"));
    compare.append(
        Some("Compare: scroll together"),
        Some("win.compare-toggle-sync"),
    );
    compare.append(
        Some("Compare: show differences"),
        Some("win.compare-toggle-diff"),
    );
    menu.append_section(None, &compare);

    let queue = gio::Menu::new();
    queue.append(Some("Cancel waiting jobs"), Some("win.cancel-waiting"));
    menu.append_section(None, &queue);

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

type Handler = fn(&Rc<App>);

/// Every window action and what it does. One table, so the tests can check
/// that each tool card, shortcut and enable-list names an action that exists.
fn handlers() -> Vec<(&'static str, Handler)> {
    vec![
        ("open", |app| app.choose_file_to_open()),
        ("save", |app| app.save()),
        ("save-as", |app| app.save_as()),
        ("close", |app| app.ui.window.close()),
        ("undo", |app| app.step(OpenMode::Undo)),
        ("redo", |app| app.step(OpenMode::Redo)),
        ("rotate-left", |app| app.rotate(-90)),
        ("rotate-right", |app| app.rotate(90)),
        ("delete", |app| app.delete_pages()),
        ("select-all", |app| app.select_all()),
        ("zoom-in", |app| app.zoom_by(1)),
        ("zoom-out", |app| app.zoom_by(-1)),
        ("zoom-fit", |app| app.set_zoom(Zoom::FitWidth)),
        ("dashboard", |app| app.toggle_dashboard()),
        ("shortcuts", |app| app.show_shortcuts()),
        ("about", |app| app.show_about()),
        ("cancel-waiting", |app| app.cancel_waiting()),
        // The tools, in the order of the dashboard.
        ("merge", |app| app.pick_merge()),
        ("split", |app| app.split_every()),
        ("compress-screen", |app| app.compress(CompressLevel::Screen)),
        ("compress-print", |app| app.compress(CompressLevel::Print)),
        ("ocr", |app| app.run_ocr()),
        ("export-text", |app| app.export_text()),
        ("export-html", |app| app.export_html()),
        ("from-images", |app| app.from_images()),
        ("export-images", |app| app.export_images()),
        ("from-office", |app| app.from_office()),
        ("pdf-to-office", |app| app.to_office()),
        ("from-text", |app| app.from_text()),
        ("from-markdown", |app| app.from_markdown()),
        ("encrypt", |app| app.encrypt()),
        ("stamp", |app| app.stamp()),
        ("page-numbers", |app| app.add_page_numbers()),
        ("crop", |app| app.crop()),
        ("redact", |app| app.redact()),
        ("flatten", |app| app.flatten()),
        ("sign", |app| app.sign()),
        ("linearize", |app| app.linearize()),
        ("pdfa", |app| app.pdf_a()),
        ("check-pdfa", |app| app.check_pdf_a()),
        ("bookmarks", |app| app.bookmarks()),
        ("n-up", |app| app.n_up()),
        ("booklet", |app| app.booklet()),
        ("halve", |app| app.halve()),
        ("fill-form", |app| app.fill_form()),
        ("verify-signatures", |app| app.verify_signatures()),
        ("new-blank", |app| app.new_blank()),
        ("qr-code", |app| app.qr_code()),
        ("to-svg", |app| app.to_svg()),
        ("to-markdown", |app| app.to_markdown()),
        ("attach", |app| app.attach_file()),
        ("layers", |app| app.layers()),
        ("page-labels", |app| app.page_labels()),
        ("list-attachments", |app| app.list_attachments()),
        ("list-layers", |app| app.list_layers()),
        // The compare view.
        ("compare-open-left", |app| {
            app.choose_file_for_compare(ComparisonSide::Left)
        }),
        ("compare-open-right", |app| {
            app.choose_file_for_compare(ComparisonSide::Right)
        }),
        ("compare-toggle-sync", |app| app.toggle_compare_sync()),
        ("compare-toggle-diff", |app| app.toggle_compare_diff()),
        ("compare-focus-left", |app| {
            app.focus_compare_side(ComparisonSide::Left)
        }),
        ("compare-focus-right", |app| {
            app.focus_compare_side(ComparisonSide::Right)
        }),
    ]
}

mod stage8;

fn add_actions(app: &Rc<App>) {
    for (name, handler) in handlers() {
        add_action(app, name, handler);
    }
}

fn add_action(app: &Rc<App>, name: &'static str, handler: Handler) {
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

/// Wire the compare view: per-side buttons, zoom with Ctrl+scroll, resize
/// refits, and scroll that follows across sides while sync is on.
fn add_compare_controllers(app: &Rc<App>) {
    app.ui.compare_sync_button.connect_clicked(clone!(
        #[weak]
        app,
        move |_| app.toggle_compare_sync()
    ));
    app.ui.compare_diff_button.connect_clicked(clone!(
        #[weak]
        app,
        move |_| app.toggle_compare_diff()
    ));

    for side in [ComparisonSide::Left, ComparisonSide::Right] {
        let bar = app.compare_bar(side).clone();
        let viewer = app.compare_viewer(side).clone();

        bar.prev.connect_clicked(clone!(
            #[weak]
            app,
            move |_| app.compare_step(side, -1)
        ));
        bar.next.connect_clicked(clone!(
            #[weak]
            app,
            move |_| app.compare_step(side, 1)
        ));
        bar.zoom_in.connect_clicked(clone!(
            #[weak]
            app,
            move |_| app.zoom_compare_by(side, 1)
        ));
        bar.zoom_out.connect_clicked(clone!(
            #[weak]
            app,
            move |_| app.zoom_compare_by(side, -1)
        ));
        bar.fit.connect_clicked(clone!(
            #[weak]
            app,
            move |_| app.set_compare_zoom(side, Zoom::FitWidth)
        ));

        // A wider or narrower viewer means a fitted page needs a new size.
        viewer.hadjustment().connect_notify_local(
            Some("page-size"),
            clone!(
                #[weak]
                app,
                move |_, _| app.schedule_compare_refit()
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
                if !ctrl || dy == 0.0 || !app.compare_state.borrow().side(side).is_open() {
                    return glib::Propagation::Proceed;
                }
                app.zoom_compare_by(side, if dy < 0.0 { 1 } else { -1 });
                glib::Propagation::Stop
            }
        ));
        viewer.add_controller(scroll);
    }

    // While sync is on, moving one viewer moves the other. The flag stops the
    // change we make from bouncing straight back.
    let guard = Rc::new(Cell::new(false));
    let link = |from: gtk4::Adjustment, to: gtk4::Adjustment| {
        from.connect_value_changed(clone!(
            #[strong]
            guard,
            #[weak]
            app,
            move |from| {
                if guard.get() {
                    return;
                }
                let linked = {
                    let state = app.compare_state.borrow();
                    state.sync && state.both_open()
                };
                if linked {
                    guard.set(true);
                    to.set_value(from.value());
                    guard.set(false);
                }
            }
        ));
    };
    let left = &app.ui.compare_left_viewer;
    let right = &app.ui.compare_right_viewer;
    link(left.hadjustment(), right.hadjustment());
    link(left.vadjustment(), right.vadjustment());
    link(right.hadjustment(), left.hadjustment());
    link(right.vadjustment(), left.vadjustment());
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
                // An open or an undo holds the worker with the sentinel and
                // frees it here. A merge reports its result the same way but
                // is a queued job, which finishes with its own message.
                if self.busy_job.get() == Some(u64::MAX) {
                    self.finish_current();
                }
                let message = self.opened(path, page_count);
                self.generation.set(self.generation.get() + 1);
                self.show_document();
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

            Response::CompareOpened {
                path,
                page_count,
                slot,
            } => {
                if self.busy_job.get() == Some(u64::MAX) {
                    self.finish_current();
                }
                self.compare_pending.set(None);
                let side = side_of(slot);
                {
                    let mut compare = self.compare_state.borrow_mut();
                    match side {
                        ComparisonSide::Left => compare.open_left(path.clone(), page_count),
                        ComparisonSide::Right => compare.open_right(path.clone(), page_count),
                    }
                }
                // Renders still in flight were for what was there before.
                self.compare_generation
                    .set(self.compare_generation.get() + 1);
                *self.compare_images.borrow_mut() = (None, None);
                self.show_compare();

                let sides = if self.compare_state.borrow().both_open() {
                    "Comparing the two documents.".to_string()
                } else {
                    format!(
                        "Opened {} on the {} side. Open another PDF for the other side.",
                        file_name(&path),
                        side_name(side)
                    )
                };
                self.set_status(&sides);
            }

            Response::CompareRendered {
                page,
                generation,
                purpose,
                dpi,
                image,
                slot,
            } => {
                if generation != self.compare_generation.get() {
                    return;
                }
                let Purpose::View { ticket } = purpose else {
                    return;
                };
                let side = side_of(slot);
                let current_ticket = match side {
                    ComparisonSide::Left => self.compare_left_ticket.get(),
                    ComparisonSide::Right => self.compare_right_ticket.get(),
                };
                let wanted = {
                    let compare = self.compare_state.borrow();
                    let state = compare.side(side);
                    state.source_page(state.current)
                };
                if ticket != current_ticket || wanted != Some(page) {
                    return;
                }

                match side {
                    ComparisonSide::Left => {
                        self.compare_left_dpi.set(dpi);
                        self.compare_images.borrow_mut().0 = Some(image);
                    }
                    ComparisonSide::Right => {
                        self.compare_right_dpi.set(dpi);
                        self.compare_images.borrow_mut().1 = Some(image);
                    }
                }
                self.paint_compare();
            }

            Response::Report { title, body } => {
                stage8::show_report(&self.ui.window, &title, &body);
            }
            Response::Progress(message) => {
                self.ui.progress.set_visible(true);
                // A determinate fraction arrives on JobProgress; the string
                // pump stays indeterminate.
                if self.current_fraction().is_none() {
                    self.ui.progress.pulse();
                }
                self.set_status(&message);
            }

            Response::JobProgress {
                job_id,
                fraction,
                message,
            } => {
                if self.busy_job.get() != Some(job_id) {
                    return;
                }
                self.ui.progress.set_visible(true);
                match fraction {
                    Some(done) => self.ui.progress.set_fraction(done.clamp(0.0, 1.0) as f64),
                    None => self.ui.progress.pulse(),
                }
                self.queue.borrow_mut().mark_progress(job_id, fraction);
                self.set_status(&message);
                self.refresh_queue();
            }

            Response::JobFinished { job_id, message } => {
                // The worker sends `Finished` just before this, and that
                // already closed the job, so this is normally a repeat.
                if self.busy_job.get() != Some(job_id) {
                    return;
                }
                self.queue.borrow_mut().mark_done(job_id, message.clone());
                self.finish_current();
                self.set_status(&message);
            }

            Response::SourceMoved { from, to } => {
                self.state.borrow_mut().relocate_source(&from, to);
            }

            Response::Saved { path, unprotected } => {
                self.finish_current();
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
                let current = self.busy_job.get();
                if let Some(job_id) = current {
                    self.queue.borrow_mut().mark_done(job_id, message.clone());
                }
                self.finish_current();
                self.set_status(&message);
            }

            Response::Failed { task, error } => {
                let current = self.busy_job.get();
                if task != Task::Render {
                    if let Some(job_id) = current {
                        // Batch failures belong to the running job; immediate
                        // ones (open, save) hold the sentinel instead.
                        if self.queue.borrow().get(job_id).is_some() {
                            self.queue
                                .borrow_mut()
                                .mark_failed(job_id, error.to_string());
                            self.refresh_queue();
                        }
                    }
                    self.finish_current();
                }
                if task == Task::Save {
                    self.saving.replace(None);
                    self.after_save.replace(None);
                }
                if matches!(task, Task::Open | Task::Merge | Task::CompareOpen) {
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
                            self.compare_pending.set(None);
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
        self.set_zoom(Zoom::Factor(next_zoom(now, direction)));
    }

    // ==================== COMPARE VIEW METHODS ====================

    fn compare_bar(&self, side: ComparisonSide) -> &SideBar {
        match side {
            ComparisonSide::Left => &self.ui.compare_left_bar,
            ComparisonSide::Right => &self.ui.compare_right_bar,
        }
    }

    fn compare_viewer(&self, side: ComparisonSide) -> &ScrolledWindow {
        match side {
            ComparisonSide::Left => &self.ui.compare_left_viewer,
            ComparisonSide::Right => &self.ui.compare_right_viewer,
        }
    }

    fn compare_picture(&self, side: ComparisonSide) -> &Picture {
        match side {
            ComparisonSide::Left => &self.ui.compare_left_page,
            ComparisonSide::Right => &self.ui.compare_right_page,
        }
    }

    fn compare_zoom(&self, side: ComparisonSide) -> &Cell<Zoom> {
        match side {
            ComparisonSide::Left => &self.compare_left_zoom,
            ComparisonSide::Right => &self.compare_right_zoom,
        }
    }

    fn compare_dpi(&self, side: ComparisonSide) -> &Cell<f32> {
        match side {
            ComparisonSide::Left => &self.compare_left_dpi,
            ComparisonSide::Right => &self.compare_right_dpi,
        }
    }

    /// Open a file chooser for the compare view on the given side.
    fn choose_file_for_compare(self: &Rc<Self>, side: ComparisonSide) {
        let filter = gtk4::FileFilter::new();
        filter.set_name(Some("PDF documents"));
        filter.add_mime_type("application/pdf");
        filter.add_pattern("*.pdf");

        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);

        let dialog = FileDialog::builder()
            .title(format!("Open a PDF for the {} side", side_name(side)))
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
                            app.open_compare(side, path, None);
                        }
                    }
                }
            ),
        );
    }

    /// Open a document on one side of the compare view.
    fn open_compare(
        self: &Rc<Self>,
        side: ComparisonSide,
        path: PathBuf,
        password: Option<String>,
    ) {
        if self.refuse_while_busy() {
            return;
        }
        self.compare_pending.set(Some(side));
        self.start(&format!("Opening {} for comparison…", file_name(&path)));
        let _ = self.to_worker.send(Request::CompareOpen {
            path,
            password,
            slot: slot_of(side),
        });
    }

    /// Show the compare view and draw both sides.
    fn show_compare(self: &Rc<Self>) {
        self.ui.stack.set_visible_child_name("compare");
        self.ui
            .compare_sync_button
            .set_active(self.compare_state.borrow().sync);
        self.ui
            .compare_diff_button
            .set_active(self.compare_diff.get());
        self.refresh_compare_bars();
        self.refresh_actions();
        self.show_compare_current();
    }

    /// Names and page counts above each side.
    fn refresh_compare_bars(self: &Rc<Self>) {
        let compare = self.compare_state.borrow();
        for side in [ComparisonSide::Left, ComparisonSide::Right] {
            let state = compare.side(side);
            let bar = self.compare_bar(side);
            if state.is_open() {
                let name = state.display_name().unwrap_or_default();
                bar.title
                    .set_label(&format!("{}: {name}", side_name_title(side)));
                bar.pages.set_label(&format!(
                    "{} / {}",
                    state.current + 1,
                    state.visible_pages()
                ));
            } else {
                bar.title
                    .set_label(&format!("{}: nothing open yet", side_name_title(side)));
                bar.pages.set_label("–");
            }
            let has_pages = state.is_open();
            bar.prev.set_sensitive(has_pages && state.current > 0);
            bar.next
                .set_sensitive(has_pages && state.current + 1 < state.visible_pages());
            bar.zoom_in.set_sensitive(has_pages);
            bar.zoom_out.set_sensitive(has_pages);
            bar.fit.set_sensitive(has_pages);
        }
    }

    /// Request renders for both sides of the compare view.
    fn show_compare_current(self: &Rc<Self>) {
        self.refresh_compare_bars();

        for side in [ComparisonSide::Left, ComparisonSide::Right] {
            let page = {
                let compare = self.compare_state.borrow();
                let state = compare.side(side);
                state.source_page(state.current)
            };

            let Some(page) = page else {
                self.compare_picture(side)
                    .set_paintable(None::<&gdk::Texture>);
                continue;
            };

            let ticket_cell = match side {
                ComparisonSide::Left => &self.compare_left_ticket,
                ComparisonSide::Right => &self.compare_right_ticket,
            };
            let ticket = ticket_cell.get() + 1;
            ticket_cell.set(ticket);

            let _ = self
                .to_worker
                .send(Request::CompareRender(CompareRenderRequest {
                    page,
                    size: self.compare_size(side),
                    purpose: Purpose::View { ticket },
                    generation: self.compare_generation.get(),
                    slot: slot_of(side),
                }));
        }
    }

    /// Whether the two viewers must draw at one scale: when they scroll
    /// together, or when differences are being marked, the pages have to line
    /// up pixel for pixel.
    fn compare_locked(&self) -> bool {
        self.compare_state.borrow().sync || self.compare_diff.get()
    }

    /// The render size for one compare viewer.
    fn compare_size(self: &Rc<Self>, side: ComparisonSide) -> Size {
        let locked = self.compare_locked();
        // A locked pair follows the left side's zoom.
        let zoom = if locked {
            self.compare_left_zoom.get()
        } else {
            self.compare_zoom(side).get()
        };

        match zoom {
            Zoom::Factor(factor) => Size::Dpi(ZOOM_100_DPI * factor),
            Zoom::FitWidth => {
                let width = if locked {
                    self.ui
                        .compare_left_viewer
                        .width()
                        .min(self.ui.compare_right_viewer.width())
                } else {
                    self.compare_viewer(side).width()
                };
                let available = width - 2 * VIEW_MARGIN;
                if available > 0 {
                    Size::Width {
                        pixels: available.max(64) as u32,
                        quarter_turned: false,
                    }
                } else {
                    // Not laid out yet; the resize that follows will refit.
                    Size::Dpi(ZOOM_100_DPI)
                }
            }
        }
    }

    /// Draw what has arrived: each side's page, or on the right the
    /// differences from the left when that is switched on.
    fn paint_compare(self: &Rc<Self>) {
        let (left, right) = self.compare_images.borrow().clone();

        if let Some(left) = &left {
            self.ui
                .compare_left_page
                .set_paintable(Some(&texture(left, 0)));
        }

        let Some(right) = right else { return };
        let mut title_note = String::new();
        let mut shown = None;

        if self.compare_diff.get() {
            if let Some(left) = &left {
                match diff(left, &right) {
                    Some(result) => {
                        title_note = if result.changed == 0 {
                            " (identical)".to_string()
                        } else {
                            format!(" ({:.1}% differs)", result.percent())
                        };
                        shown = Some(texture(&result.overlay, 0));
                    }
                    None => {
                        title_note = " (sizes differ, cannot mark)".to_string();
                    }
                }
            }
        }

        let texture = shown.unwrap_or_else(|| texture(&right, 0));
        self.ui.compare_right_page.set_paintable(Some(&texture));

        let compare = self.compare_state.borrow();
        if let Some(name) = compare.right.display_name() {
            self.ui.compare_right_bar.title.set_label(&format!(
                "{}: {name}{title_note}",
                side_name_title(ComparisonSide::Right)
            ));
        }
    }

    /// Schedule a re-render of the compare view after a resize, once the size
    /// has stopped changing.
    fn schedule_compare_refit(self: &Rc<Self>) {
        let fitted = self.compare_locked() && self.compare_left_zoom.get() == Zoom::FitWidth
            || self.compare_left_zoom.get() == Zoom::FitWidth
            || self.compare_right_zoom.get() == Zoom::FitWidth;
        if !fitted
            || !self.compare_state.borrow().left.is_open()
                && !self.compare_state.borrow().right.is_open()
        {
            return;
        }
        if let Some(pending) = self.compare_refit.take() {
            pending.remove();
        }
        let id = glib::timeout_add_local_once(
            Duration::from_millis(120),
            clone!(
                #[weak(rename_to = app)]
                self,
                move || {
                    app.compare_refit.replace(None);
                    app.show_compare_current();
                }
            ),
        );
        self.compare_refit.replace(Some(id));
    }

    /// Set zoom for a compare side, or both while they are locked together.
    fn set_compare_zoom(self: &Rc<Self>, side: ComparisonSide, zoom: Zoom) {
        if self.compare_locked() {
            self.compare_left_zoom.set(zoom);
            self.compare_right_zoom.set(zoom);
        } else {
            self.compare_zoom(side).set(zoom);
        }
        self.show_compare_current();
    }

    /// Step one side's zoom to the next level up (positive) or down.
    fn zoom_compare_by(self: &Rc<Self>, side: ComparisonSide, direction: i32) {
        let now = self.compare_dpi(side).get() / ZOOM_100_DPI;
        self.set_compare_zoom(side, Zoom::Factor(next_zoom(now, direction)));
    }

    /// Move one side to another page; with sync on, the other side follows.
    fn compare_step(self: &Rc<Self>, side: ComparisonSide, direction: i32) {
        {
            let mut compare = self.compare_state.borrow_mut();
            let at = compare.side(side).current as i64 + direction as i64;
            compare.set_current(side, at.max(0) as usize);
        }
        // The next page may be a different size; start from the top of it.
        self.compare_viewer(side).vadjustment().set_value(0.0);
        self.show_compare_current();
    }

    /// Switch scrolling together on or off.
    fn toggle_compare_sync(self: &Rc<Self>) {
        let sync = self.compare_state.borrow_mut().toggle_sync();
        self.ui.compare_sync_button.set_active(sync);

        if sync {
            // Bring the right side to where the left is.
            let left_page = self.compare_state.borrow().left.current;
            self.compare_state
                .borrow_mut()
                .set_current(ComparisonSide::Left, left_page);
            let zoom = self.compare_left_zoom.get();
            self.compare_right_zoom.set(zoom);
            self.ui
                .compare_right_viewer
                .hadjustment()
                .set_value(self.ui.compare_left_viewer.hadjustment().value());
            self.ui
                .compare_right_viewer
                .vadjustment()
                .set_value(self.ui.compare_left_viewer.vadjustment().value());
            self.set_status("Scrolling, zooming and page changes now go together.");
        } else {
            self.set_status("The two sides now move on their own.");
        }
        self.show_compare_current();
    }

    /// Switch the difference marks on or off.
    fn toggle_compare_diff(self: &Rc<Self>) {
        let on = !self.compare_diff.get();
        self.compare_diff.set(on);
        self.ui.compare_diff_button.set_active(on);

        if on {
            let zoom = self.compare_left_zoom.get();
            self.compare_right_zoom.set(zoom);
            self.set_status("Marking differences in red on the right side.");
            // Different scale would make every pixel differ, so draw both
            // sides afresh at one size.
            self.show_compare_current();
        } else {
            self.set_status("Difference marks are off.");
            self.show_compare_current();
        }
    }

    /// Focus a compare side (Ctrl+1 for left, Ctrl+2 for right).
    fn focus_compare_side(self: &Rc<Self>, side: ComparisonSide) {
        self.compare_viewer(side).grab_focus();
        self.set_status(&format!("Focused the {} viewer.", side_name(side)));
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
        // Merging opens its result, so the dashboard gives way to it.
        self.show_document();
        let detail = output.display().to_string();
        self.submit("Merge", detail, move |job_id| Request::Merge {
            inputs,
            output,
            unlock,
            job_id,
        });
    }

    /// An open, a merge or a compare open stopped at a file that needs a
    /// password. Ask for it, then try again; `wrong` says the last password
    /// given did not work.
    fn ask_to_unlock(self: &Rc<Self>, task: Task, path: PathBuf, wrong: bool) {
        let merge = if task == Task::Merge {
            match self.pending_merge.take() {
                Some(merge) => Some(merge),
                None => return,
            }
        } else {
            None
        };
        let compare_side = if task == Task::CompareOpen {
            match self.compare_pending.take() {
                Some(side) => Some(side),
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

                    if let Some((inputs, output)) = merge {
                        app.merge(inputs, output, Some((locked, password)));
                    } else if let Some(side) = compare_side {
                        app.open_compare(side, locked, Some(password));
                    } else {
                        if app.refuse_while_busy() {
                            return;
                        }
                        // The open mode is still that of the attempt that
                        // asked for the password.
                        app.start("Unlocking…");
                        let _ = app.to_worker.send(Request::Open {
                            path: locked,
                            password: Some(password),
                        });
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

    /// Ask where to write a PDF, then queue the job `make` builds for it. The
    /// edits are taken when the place is chosen, so what is written is what
    /// the window shows at that moment.
    fn write_pdf_job(
        self: &Rc<Self>,
        suffix: &str,
        label: &'static str,
        make: impl FnOnce(PathBuf, Edits, u64) -> Request + 'static,
    ) {
        let name = self.state.borrow().suggested_name(suffix, "pdf");
        self.ask_for_file(&name, move |app, destination| {
            let edits = app.state.borrow().edits();
            let detail = destination.display().to_string();
            app.submit(label, detail, move |job_id| {
                make(destination, edits, job_id)
            });
        });
    }

    /// Ask for a folder, then hand it to `then`.
    fn ask_for_folder(
        self: &Rc<Self>,
        title: &str,
        then: impl FnOnce(&Rc<Self>, PathBuf) + 'static,
    ) {
        let dialog = FileDialog::builder().title(title).build();
        dialog.select_folder(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    if let Ok(folder) = result {
                        if let Some(path) = folder.path() {
                            then(&app, path);
                        }
                    }
                }
            ),
        );
    }

    fn compress(self: &Rc<Self>, level: CompressLevel) {
        if !self.require_document_queued() {
            return;
        }
        let label = match level {
            CompressLevel::Screen => "Compress for screen",
            _ => "Compress for print",
        };
        self.write_pdf_job("-compressed", label, move |path, edits, job_id| {
            Request::Compress {
                path,
                level,
                edits,
                job_id,
            }
        });
    }

    fn run_ocr(self: &Rc<Self>) {
        if !self.require_document_queued() {
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

        show_form(
            &self.ui.window,
            "Make searchable",
            "Choose where to save…",
            |fields| {
                let language = text_entry("eng");
                language.set_tooltip_text(Some(
                    "A Tesseract language code such as eng or deu. Join several with +, like eng+deu.",
                ));
                fields.add("Language", &language);
                fields.note(
                    "Tesseract reads each page as a picture and adds the text underneath. \
                     The language must be installed for Tesseract.",
                );
                Box::new(move || {
                    let code = language.text().trim().to_string();
                    let valid = !code.is_empty()
                        && code
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+');
                    if valid {
                        Ok(code)
                    } else {
                        Err("Enter a language code such as eng, or eng+deu for two.".to_string())
                    }
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |language: String| {
                    app.write_pdf_job(
                        "-searchable",
                        "Make searchable",
                        move |path, edits, job_id| Request::Ocr {
                            path,
                            language,
                            edits,
                            job_id,
                        },
                    );
                }
            ),
        );
    }

    fn export_images(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        show_form(
            &self.ui.window,
            "Export pages as images",
            "Choose where to save…",
            |fields| {
                let format = dropdown(&["PNG (sharp, larger)", "JPEG (smaller)"]);
                let dpi = spin(150.0, 72.0, 600.0, 10.0);
                let output = dropdown(&["A folder of images", "One ZIP file"]);
                fields.add("Format", &format);
                fields.add("Resolution (dpi)", &dpi);
                fields.add("Save as", &output);
                fields.note("150 dpi suits the screen. Use 300 for print.");
                Box::new(move || {
                    let format = if format.selected() == 1 {
                        ImageFormat::Jpeg
                    } else {
                        ImageFormat::Png
                    };
                    Ok((format, dpi.value() as f32, output.selected() == 1))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(format, dpi, zip): (ImageFormat, f32, bool)| {
                    let queue = move |app: &Rc<App>, output: ImageOutput| {
                        let edits = app.state.borrow().edits();
                        let detail = output.display();
                        app.submit("Export images", detail, move |job_id| {
                            Request::ExportImages {
                                output,
                                dpi,
                                format,
                                edits,
                                job_id,
                            }
                        });
                    };
                    if zip {
                        let name = app.state.borrow().suggested_name("-pages", "zip");
                        app.ask_for_file(&name, move |app, path| {
                            queue(app, ImageOutput::Zip(path))
                        });
                    } else {
                        app.ask_for_folder("Choose a folder for the images", move |app, path| {
                            queue(app, ImageOutput::Folder(path))
                        });
                    }
                }
            ),
        );
    }

    /// Pick several PDFs and a destination, then queue the merge.
    fn pick_merge(self: &Rc<Self>) {
        let filter = gtk4::FileFilter::new();
        filter.set_name(Some("PDF documents"));
        filter.add_mime_type("application/pdf");
        filter.add_pattern("*.pdf");

        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);

        let dialog = FileDialog::builder()
            .title("Choose the PDFs to merge")
            .filters(&filters)
            .build();

        dialog.open_multiple(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(files) = result else { return };
                    let paths = paths_of(&files);
                    if paths.len() < 2 {
                        app.set_status("Choose at least two PDFs to merge.");
                        return;
                    }
                    app.ask_for_merge_output(paths);
                }
            ),
        );
    }

    /// Ask where the merged file goes, then merge.
    fn ask_for_merge_output(self: &Rc<Self>, paths: Vec<PathBuf>) {
        let save = FileDialog::builder()
            .title("Save the merged document")
            .initial_name("merged.pdf")
            .build();
        save.save(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(output) = file.path() {
                            app.merge(paths, output, None);
                        }
                    }
                }
            ),
        );
    }

    /// Burst the open document into pieces of a chosen length.
    fn split_every(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let count = self.state.borrow().visible_pages().max(1);

        show_form(
            &self.ui.window,
            "Split",
            "Choose a folder…",
            |fields| {
                let every = spin(1.0, 1.0, count as f64, 1.0);
                fields.add("Pages per file", &every);
                fields.note(&format!(
                    "The document has {count} page{}. Each piece is saved in the folder you pick.",
                    if count == 1 { "" } else { "s" }
                ));
                Box::new(move || Ok(every.value_as_int().max(1) as usize))
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |every: usize| {
                    app.ask_for_folder("Choose a folder for the pieces", move |app, directory| {
                        let edits = app.state.borrow().edits();
                        let detail = directory.display().to_string();
                        app.submit("Split", detail, move |job_id| Request::Split {
                            directory,
                            every,
                            edits,
                            job_id,
                        });
                    });
                }
            ),
        );
    }

    /// Stamp text across the chosen pages.
    fn stamp(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let count = self.state.borrow().visible_pages();

        show_form(
            &self.ui.window,
            "Watermark",
            "Choose where to save…",
            |fields| {
                let text = text_entry("DRAFT");
                let pages = pages_entry();
                let position = dropdown(&ANCHOR_NAMES);
                position.set_selected(4);
                let size = spin(72.0, 8.0, 300.0, 4.0);
                let opacity = spin(30.0, 5.0, 100.0, 5.0);
                let turn = spin(45.0, -180.0, 180.0, 5.0);
                let layer = dropdown(&["On top of the page", "Behind the page"]);
                fields.add("Text", &text);
                fields.add("Pages", &pages);
                fields.add("Position", &position);
                fields.add("Size (points)", &size);
                fields.add("Opacity (%)", &opacity);
                fields.add("Turn (degrees)", &turn);
                fields.add("Layer", &layer);
                Box::new(move || {
                    let words = text.text().trim().to_string();
                    if words.is_empty() {
                        return Err("Type the text to stamp.".to_string());
                    }
                    let mut options = StampOptions::text(words);
                    options.anchor = ANCHORS[position.selected() as usize];
                    options.font_size = size.value() as f32;
                    options.opacity = opacity.value() as f32 / 100.0;
                    options.rotation_deg = turn.value() as f32;
                    options.color = (0.55, 0.55, 0.55);
                    options.font = StampFont::Helvetica;
                    options.layer = if layer.selected() == 1 {
                        StampLayer::Under
                    } else {
                        StampLayer::Over
                    };
                    Ok((options, pages_from(&pages, count)?))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(options, pages): (StampOptions, PageRange)| {
                    app.stamp_job("-stamped", "Watermark", Task::Stamp, options, pages);
                }
            ),
        );
    }

    /// Ask where to write, then queue a stamp job.
    fn stamp_job(
        self: &Rc<Self>,
        suffix: &str,
        label: &'static str,
        task: Task,
        options: StampOptions,
        pages: PageRange,
    ) {
        self.write_pdf_job(suffix, label, move |path, edits, job_id| Request::Stamp {
            path,
            options: Box::new(options),
            pages,
            task,
            edits,
            job_id,
        });
    }

    /// Sign the document with a drawn or a typed signature.
    fn sign(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let count = self.state.borrow().visible_pages();

        show_form(
            &self.ui.window,
            "Sign",
            "Choose where to save…",
            |fields| {
                let strokes: Strokes = Rc::new(RefCell::new(Vec::new()));
                let pad = signature_pad(&strokes);
                let clear = Button::with_label("Clear");
                clear.set_halign(Align::End);
                clear.connect_clicked(clone!(
                    #[strong]
                    strokes,
                    #[weak]
                    pad,
                    move |_| {
                        strokes.borrow_mut().clear();
                        pad.queue_draw();
                    }
                ));
                fields.wide(&pad);
                fields.wide(&clear);

                let typed = text_entry("");
                typed.set_placeholder_text(Some("Or type your name"));
                let pages = pages_entry();
                pages.set_text(&count.to_string());
                let position = dropdown(&ANCHOR_NAMES);
                position.set_selected(8);
                let width = spin(160.0, 40.0, 400.0, 10.0);
                fields.add("Typed", &typed);
                fields.add("Pages", &pages);
                fields.add("Position", &position);
                fields.add("Width (points)", &width);
                fields.note(
                    "Draw with the mouse or a touchscreen. A drawing is used when there is one; \
                     the typed name is used only when the pad is empty.",
                );

                Box::new(move || {
                    let anchor = ANCHORS[position.selected() as usize];
                    let range = pages_from(&pages, count)?;
                    let mut options = StampOptions {
                        anchor,
                        ..StampOptions::default()
                    };

                    let drawn = signature_strokes(&strokes.borrow(), width.value() as f32);
                    if let Some(drawn) = drawn {
                        options.content = StampContent::Signature(drawn);
                        options.stroke_width = 1.8;
                        options.stroke_color = (0.1, 0.1, 0.1);
                    } else {
                        let name = typed.text().trim().to_string();
                        if name.is_empty() {
                            return Err("Draw your signature, or type your name.".to_string());
                        }
                        options.content = StampContent::Text(name);
                        options.font_size = 28.0;
                    }
                    Ok((options, range))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(options, pages): (StampOptions, PageRange)| {
                    app.stamp_job("-signed", "Sign", Task::Sign, options, pages);
                }
            ),
        );
    }

    /// Crop by margins or to an exact box, scale to a standard size, or undo an
    /// earlier crop.
    fn crop(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let count = self.state.borrow().visible_pages();

        // What the main view shows: the page as the user sees it, with its
        // size in points worked out from the render resolution. It gives the
        // preview its picture and lets the form check margins against a real
        // page before the job starts.
        let shown = self.ui.page_view.paintable().filter(|paintable| {
            paintable.intrinsic_width() > 0 && paintable.intrinsic_height() > 0
        });
        let page_points = shown.as_ref().map(|paintable| {
            let to_points = 72.0 / f64::from(self.view_dpi.get().max(1.0));
            (
                f64::from(paintable.intrinsic_width()) * to_points,
                f64::from(paintable.intrinsic_height()) * to_points,
            )
        });

        show_form(
            &self.ui.window,
            "Crop or resize",
            "Choose where to save…",
            |fields| {
                let mode = dropdown(&[
                    "Crop by margins",
                    "Crop to an exact box",
                    "Scale to a page size",
                    "Remove an earlier crop",
                ]);
                let pages = pages_entry();
                fields.add("What to do", &mode);
                fields.add("Pages", &pages);

                // Margins: the same amount off every selected page, however
                // big each one is.
                let unit = dropdown(&[
                    Unit::Points.name(),
                    Unit::Millimetres.name(),
                    Unit::Inches.name(),
                ]);
                let margin_spin = |points: f64| {
                    let unit = Unit::Points;
                    let button = spin(points, 0.0, 5000.0, unit.step());
                    button.set_digits(unit.digits());
                    button
                };
                let top = margin_spin(36.0);
                let bottom = margin_spin(36.0);
                let left = margin_spin(36.0);
                let right = margin_spin(36.0);
                // The margin fields sit in a column beside the preview.
                let margin_fields = Fields {
                    content: GtkBox::new(Orientation::Vertical, 10),
                };
                margin_fields.add("Units", &unit);
                margin_fields.add("Top margin", &top);
                margin_fields.add("Bottom margin", &bottom);
                margin_fields.add("Left margin", &left);
                margin_fields.add("Right margin", &right);
                let spins = [top.clone(), bottom.clone(), left.clone(), right.clone()];
                let current_unit = Rc::new(Cell::new(Unit::Points));
                unit.connect_selected_notify(clone!(
                    #[strong]
                    current_unit,
                    #[strong]
                    spins,
                    move |unit| {
                        // Keep the same amount of paper, shown in the new unit.
                        let (before, after) =
                            (current_unit.get(), Unit::from_index(unit.selected()));
                        for button in &spins {
                            let points = before.to_points(button.value());
                            button.set_digits(after.digits());
                            button.set_increments(after.step(), after.step() * 5.0);
                            button.set_range(0.0, after.points_in(5000.0));
                            button.set_value(after.points_in(points));
                        }
                        current_unit.set(after);
                    }
                ));

                let read_margins = {
                    let spins = spins.clone();
                    let current_unit = current_unit.clone();
                    move || {
                        let unit = current_unit.get();
                        MarginValues {
                            top: unit.to_points(spins[0].value()),
                            bottom: unit.to_points(spins[1].value()),
                            left: unit.to_points(spins[2].value()),
                            right: unit.to_points(spins[3].value()),
                        }
                    }
                };

                // A small picture of the current page with the part that stays.
                let preview_area = margin_preview(shown, page_points, read_margins.clone());
                for button in &spins {
                    button.connect_value_changed(clone!(
                        #[weak]
                        preview_area,
                        move |_| preview_area.queue_draw()
                    ));
                }
                unit.connect_selected_notify(clone!(
                    #[weak]
                    preview_area,
                    move |_| preview_area.queue_draw()
                ));
                let margin_block = GtkBox::new(Orientation::Horizontal, 16);
                margin_block.append(&margin_fields.content);
                margin_block.append(&preview_area);
                fields.wide(&margin_block);
                let margin_note = fields.note(if page_points.is_some() {
                    "The preview shows the page you are looking at. Every selected page loses the same margins, measured from its own edges."
                } else {
                    "Every selected page loses the same margins, measured from its own edges."
                });

                // An exact box, for when the numbers are already known.
                let x = spin(36.0, 0.0, 5000.0, 6.0);
                let y = spin(36.0, 0.0, 5000.0, 6.0);
                let width = spin(523.0, 1.0, 5000.0, 6.0);
                let height = spin(770.0, 1.0, 5000.0, 6.0);
                let box_rows = [
                    fields.add("Left (points)", &x),
                    fields.add("Bottom (points)", &y),
                    fields.add("Width (points)", &width),
                    fields.add("Height (points)", &height),
                    fields.note("Measured from the bottom-left corner. 72 points make an inch. The same box is used on every selected page."),
                ];

                let size = dropdown(&["A4", "Letter", "Legal"]);
                let fit = dropdown(&[
                    "Fit inside, keep the shape",
                    "Fill the page, cut off the overflow",
                    "Stretch to fill",
                ]);
                let size_rows = [fields.add("Page size", &size), fields.add("Scaling", &fit)];

                on_choice(&mode, move |chosen| {
                    margin_block.set_visible(chosen == 0);
                    margin_note.set_visible(chosen == 0);
                    for row in &box_rows {
                        row.set_visible(chosen == 1);
                    }
                    for row in &size_rows {
                        row.set_visible(chosen == 2);
                    }
                });

                Box::new(move || {
                    let range = pages_from(&pages, count)?;
                    let action = match mode.selected() {
                        0 => {
                            let margins = read_margins();
                            check_margins(&margins, page_points, current_unit.get())?;
                            CropAction::Margins(Margins {
                                top: margins.top as f32,
                                bottom: margins.bottom as f32,
                                left: margins.left as f32,
                                right: margins.right as f32,
                            })
                        }
                        1 => {
                            let rect = Rect::from_xywh(
                                x.value() as f32,
                                y.value() as f32,
                                width.value() as f32,
                                height.value() as f32,
                            )
                            .map_err(|error| error.to_string())?;
                            CropAction::Box(rect)
                        }
                        2 => CropAction::ScaleTo {
                            size: match size.selected() {
                                1 => PageSize::Letter,
                                2 => PageSize::Legal,
                                _ => PageSize::A4,
                            },
                            fit: match fit.selected() {
                                1 => Fit::Cover,
                                2 => Fit::Stretch,
                                _ => Fit::Contain,
                            },
                        },
                        _ => CropAction::Reset,
                    };
                    Ok((action, range))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(action, pages): (CropAction, PageRange)| {
                    app.write_pdf_job("-cropped", "Crop", move |path, edits, job_id| {
                        Request::Crop {
                            path,
                            action,
                            pages,
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Number the pages.
    fn add_page_numbers(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let count = self.state.borrow().visible_pages();

        show_form(
            &self.ui.window,
            "Page numbers",
            "Choose where to save…",
            |fields| {
                let format = text_entry("Page {n} of {total}");
                format.set_tooltip_text(Some("{n} is the page number and {total} the page count."));
                let pages = pages_entry();
                let position = dropdown(&ANCHOR_NAMES);
                position.set_selected(7);
                let size = spin(10.0, 6.0, 48.0, 1.0);
                fields.add("Text", &format);
                fields.add("Pages", &pages);
                fields.add("Position", &position);
                fields.add("Size (points)", &size);
                fields.note("Write {n} where the page number goes and {total} for the page count.");
                Box::new(move || {
                    let template = format.text().trim().to_string();
                    if !template.contains("{n}") {
                        return Err("Include {n} so each page gets its own number.".to_string());
                    }
                    let mut options = StampOptions::text(template);
                    options.anchor = ANCHORS[position.selected() as usize];
                    options.font_size = size.value() as f32;
                    Ok((options, pages_from(&pages, count)?))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(options, pages): (StampOptions, PageRange)| {
                    app.stamp_job(
                        "-numbered",
                        "Page numbers",
                        Task::PageNumbers,
                        options,
                        pages,
                    );
                }
            ),
        );
    }

    /// Remove text, or black out a region, for good.
    fn redact(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let count = self.state.borrow().visible_pages();

        show_form(
            &self.ui.window,
            "Redact",
            "Choose where to save…",
            |fields| {
                let mode = dropdown(&["Remove matching text", "Black out a region"]);
                let pages = pages_entry();
                fields.add("What to do", &mode);
                fields.add("Pages", &pages);

                let text = text_entry("");
                let text_rows = [
                    fields.add("Text to remove", &text),
                    fields.note("Removes every piece of text that contains this, whole. A line written as one piece goes completely, not just the matching words. Case counts. No black box is drawn."),
                ];

                let x = spin(72.0, 0.0, 5000.0, 6.0);
                let y = spin(72.0, 0.0, 5000.0, 6.0);
                let width = spin(200.0, 1.0, 5000.0, 6.0);
                let height = spin(50.0, 1.0, 5000.0, 6.0);
                let region_rows = [
                    fields.add("Left (points)", &x),
                    fields.add("Bottom (points)", &y),
                    fields.add("Width (points)", &width),
                    fields.add("Height (points)", &height),
                    fields.note(
                        "Measured from the bottom-left corner. This also removes all other text \
                         on the chosen pages, so nothing can be copied out from under the box.",
                    ),
                ];

                on_choice(&mode, move |chosen| {
                    for row in &text_rows {
                        row.set_visible(chosen == 0);
                    }
                    for row in &region_rows {
                        row.set_visible(chosen == 1);
                    }
                });

                Box::new(move || {
                    let range = pages_from(&pages, count)?;
                    let target = if mode.selected() == 0 {
                        let needle = text.text().to_string();
                        if needle.trim().is_empty() {
                            return Err("Type the text to remove.".to_string());
                        }
                        RedactTarget::Text(needle)
                    } else {
                        let rect = Rect::from_xywh(
                            x.value() as f32,
                            y.value() as f32,
                            width.value() as f32,
                            height.value() as f32,
                        )
                        .map_err(|error| error.to_string())?;
                        RedactTarget::Region(rect)
                    };
                    Ok((target, range))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(target, pages): (RedactTarget, PageRange)| {
                    app.write_pdf_job("-redacted", "Redact", move |path, edits, job_id| {
                        Request::Redact {
                            path,
                            target,
                            pages,
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Bake form fields and annotations into the pages.
    fn flatten(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }
        let count = self.state.borrow().visible_pages();

        show_form(
            &self.ui.window,
            "Flatten",
            "Choose where to save…",
            |fields| {
                let what = dropdown(&["Forms and annotations", "Forms only", "Annotations only"]);
                let pages = pages_entry();
                fields.add("Flatten", &what);
                fields.add("Pages", &pages);
                fields.note("Flattened fields can no longer be filled in or edited.");
                Box::new(move || {
                    let options = FlattenOptions {
                        forms_only: what.selected() == 1,
                        annots_only: what.selected() == 2,
                    };
                    Ok((options, pages_from(&pages, count)?))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(options, pages): (FlattenOptions, PageRange)| {
                    app.write_pdf_job("-flattened", "Flatten", move |path, edits, job_id| {
                        Request::Flatten {
                            path,
                            options,
                            pages,
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    /// Write a password-protected copy.
    fn encrypt(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        show_form(
            &self.ui.window,
            "Protect with a password",
            "Choose where to save…",
            |fields| {
                let password = password_entry();
                let again = password_entry();
                let owner = password_entry();
                fields.add("Password", &password);
                fields.add("Repeat it", &again);
                fields.add("Owner password", &owner);
                fields.note(
                    "The password is needed to open the copy. The owner password is optional \
                     and guards the permissions; if you leave it empty, the first one is used.",
                );
                Box::new(move || {
                    let user = password.text().to_string();
                    if user.is_empty() {
                        return Err("Enter a password.".to_string());
                    }
                    if user != again.text() {
                        return Err("The two passwords do not match.".to_string());
                    }
                    let owner = owner.text().to_string();
                    let owner = if owner.is_empty() {
                        user.clone()
                    } else {
                        owner
                    };
                    Ok((user, owner))
                })
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |(user_password, owner_password): (String, String)| {
                    app.write_pdf_job("-protected", "Encrypt", move |path, edits, job_id| {
                        Request::Encrypt {
                            path,
                            user_password,
                            owner_password,
                            edits,
                            job_id,
                        }
                    });
                }
            ),
        );
    }

    fn export_text(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        let name = self.state.borrow().suggested_name("", "txt");
        self.ask_for_file(&name, move |app, destination| {
            let edits = app.state.borrow().edits();
            let detail = destination.display().to_string();
            app.submit("Extract text", detail, move |job_id| Request::ExportText {
                path: destination,
                edits,
                job_id,
            });
        });
    }

    fn export_html(self: &Rc<Self>) {
        if !self.require_document_queued() {
            return;
        }

        let name = self.state.borrow().suggested_name("", "html");
        self.ask_for_file(&name, move |app, destination| {
            let edits = app.state.borrow().edits();
            let detail = destination.display().to_string();
            app.submit("Extract HTML", detail, move |job_id| Request::ExportHtml {
                path: destination,
                edits,
                job_id,
            });
        });
    }

    /// Pick images, then queue building a PDF from them.
    fn from_images(self: &Rc<Self>) {
        let filter = gtk4::FileFilter::new();
        filter.set_name(Some("Images"));
        filter.add_mime_type("image/*");
        for pattern in ["*.png", "*.jpg", "*.jpeg", "*.webp", "*.tif", "*.tiff"] {
            filter.add_pattern(pattern);
        }

        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);

        let dialog = FileDialog::builder()
            .title("Choose the images")
            .filters(&filters)
            .build();

        dialog.open_multiple(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(files) = result else { return };
                    let inputs = paths_of(&files);
                    if inputs.is_empty() {
                        return;
                    }

                    app.ask_for_file("images.pdf", move |app, output| {
                        let detail = output.display().to_string();
                        app.submit("Images to PDF", detail, move |job_id| Request::FromImages {
                            inputs,
                            output,
                            job_id,
                        });
                    });
                }
            ),
        );
    }

    /// Pick an office file, then a folder for the PDF.
    fn from_office(self: &Rc<Self>) {
        if !self.office_available() {
            return;
        }

        let dialog = FileDialog::builder()
            .title("Choose the office file")
            .build();
        dialog.open(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(file) = result else { return };
                    let Some(input) = file.path() else {
                        return;
                    };

                    app.ask_for_folder("Choose a folder for the PDF", move |app, output_dir| {
                        let detail = output_dir.display().to_string();
                        app.submit("Office to PDF", detail, move |job_id| Request::FromOffice {
                            input,
                            output_dir,
                            job_id,
                        });
                    });
                }
            ),
        );
    }

    /// Say so, and return false, when LibreOffice is missing.
    fn office_available(self: &Rc<Self>) -> bool {
        if pdf_core::convert::office::is_available() {
            return true;
        }
        self.alert(
            "Office conversion is not available",
            "Foolscap uses LibreOffice for this, and it is not installed.\n\n\
             Install it with: sudo apt install libreoffice",
        );
        false
    }

    fn to_office(self: &Rc<Self>) {
        if !self.require_document_queued() || !self.office_available() {
            return;
        }

        show_form(
            &self.ui.window,
            "PDF to Office",
            "Choose a folder…",
            |fields| {
                let format = dropdown(&OFFICE_NAMES);
                fields.add("Format", &format);
                fields.note(
                    "Converting a PDF to an office format is approximate. Expect the layout \
                     and structure to differ from the original.",
                );
                Box::new(move || Ok(OFFICE_FORMATS[format.selected() as usize]))
            },
            clone!(
                #[strong(rename_to = app)]
                self,
                move |format: OfficeFormat| {
                    app.ask_for_folder(
                        "Choose a folder for the office file",
                        move |app, output_dir| {
                            let edits = app.state.borrow().edits();
                            let detail = output_dir.display().to_string();
                            app.submit("PDF to Office", detail, move |job_id| Request::ToOffice {
                                format,
                                output_dir,
                                edits,
                                job_id,
                            });
                        },
                    );
                }
            ),
        );
    }

    /// The worker reads the file: the GTK thread never does IO.
    fn from_text(self: &Rc<Self>) {
        let dialog = FileDialog::builder().title("Choose the text file").build();
        dialog.open(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(file) = result else { return };
                    let Some(input) = file.path() else {
                        return;
                    };

                    app.ask_for_file("document.pdf", move |app, output| {
                        let detail = output.display().to_string();
                        app.submit("Text to PDF", detail, move |job_id| Request::FromText {
                            input,
                            output,
                            job_id,
                        });
                    });
                }
            ),
        );
    }

    /// The worker reads the file: the GTK thread never does IO.
    fn from_markdown(self: &Rc<Self>) {
        let dialog = FileDialog::builder()
            .title("Choose the Markdown file")
            .build();
        dialog.open(
            Some(&self.ui.window),
            None::<&gio::Cancellable>,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let Ok(file) = result else { return };
                    let Some(input) = file.path() else {
                        return;
                    };

                    app.ask_for_file("document.pdf", move |app, output| {
                        let detail = output.display().to_string();
                        app.submit("Markdown to PDF", detail, move |job_id| {
                            Request::FromMarkdown {
                                input,
                                output,
                                job_id,
                            }
                        });
                    });
                }
            ),
        );
    }

    fn ask_for_file(
        self: &Rc<Self>,
        suggested: &str,
        then: impl FnOnce(&Rc<Self>, PathBuf) + 'static,
    ) {
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

    /// No batch job is on the worker and no immediate operation either.
    fn is_idle(&self) -> bool {
        self.busy_job.get().is_none()
    }

    /// The determinate fraction of the running job, if it reports one.
    fn current_fraction(&self) -> Option<f32> {
        let id = self.busy_job.get()?;
        match self.queue.borrow().get(id)?.status {
            JobStatus::Running { fraction } => fraction,
            _ => None,
        }
    }

    /// Mark the start of an immediate operation (open, save). The sentinel id
    /// never matches a queue row; it just holds the worker.
    fn start(self: &Rc<Self>, message: &str) {
        self.busy_job.set(Some(u64::MAX));
        self.ui.progress.set_visible(true);
        self.ui.progress.pulse();
        self.set_status(message);
        self.refresh_actions();
    }

    /// Queue a batch job. `build` receives the job's id and makes the request.
    /// It runs now when the worker is idle, otherwise it waits its turn;
    /// either way the worker stays serial. `detail` is what the job works on,
    /// shown when the pointer rests on its row.
    fn submit(self: &Rc<Self>, label: &str, detail: String, build: impl FnOnce(u64) -> Request) {
        let job_id = self.queue.borrow_mut().push(label, detail);
        let request = build(job_id);
        debug_assert_eq!(
            request.job_id(),
            Some(job_id),
            "a batch request must carry its job id"
        );
        self.pending.borrow_mut().push_back(request);
        self.pump_queue();

        let waiting = self.pending.borrow().len();
        if waiting > 0 {
            self.set_status(&format!("Queued {label} ({waiting} waiting)."));
        }
        self.refresh_actions();
    }

    /// Send the next waiting request if the worker is free.
    fn pump_queue(self: &Rc<Self>) {
        if !self.is_idle() {
            self.refresh_queue();
            return;
        }
        let next = self.pending.borrow_mut().pop_front();
        let Some(request) = next else {
            self.refresh_queue();
            return;
        };
        let label = request
            .job_id()
            .and_then(|id| self.queue.borrow().get(id).map(|job| job.label.clone()))
            .unwrap_or_else(|| "Working".to_string());
        if let Some(job_id) = request.job_id() {
            self.busy_job.set(Some(job_id));
            self.queue.borrow_mut().mark_running(job_id);
        } else {
            self.busy_job.set(Some(u64::MAX));
        }
        self.ui.progress.set_visible(true);
        self.ui.progress.pulse();
        self.set_status(&format!("{label}…"));
        let _ = self.to_worker.send(request);
        self.refresh_actions();
        self.refresh_queue();
    }

    /// The worker is free again; run whatever is waiting.
    fn finish_current(self: &Rc<Self>) {
        self.busy_job.set(None);
        self.ui
            .progress
            .set_visible(!self.pending.borrow().is_empty());
        self.refresh_actions();
        self.pump_queue();
        if self.is_idle() {
            self.ui.progress.set_visible(false);
            self.refresh_actions();
        }
    }

    /// Drop one job that has not started.
    fn cancel_job(self: &Rc<Self>, id: u64) {
        if self.queue.borrow_mut().cancel(id) {
            self.forget_pending(&[id]);
            self.set_status("Cancelled a waiting job.");
        }
    }

    /// Drop every job that has not started. The one running is left to finish,
    /// because the worker cannot be interrupted part way through a file.
    fn cancel_waiting(self: &Rc<Self>) {
        let ids = self.queue.borrow_mut().cancel_waiting();
        if ids.is_empty() {
            self.set_status("Nothing is waiting.");
            return;
        }
        self.forget_pending(&ids);
        self.set_status(&format!(
            "Cancelled {} waiting job{}.",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" }
        ));
    }

    /// Take cancelled jobs out of the line to the worker.
    fn forget_pending(self: &Rc<Self>, ids: &[u64]) {
        let mut merge_dropped = false;
        self.pending.borrow_mut().retain(|request| {
            let cancelled = request.job_id().is_some_and(|id| ids.contains(&id));
            if cancelled && matches!(request, Request::Merge { .. }) {
                merge_dropped = true;
            }
            !cancelled
        });
        if merge_dropped {
            // The merge was going to open its result; it will not now.
            self.open_mode.set(OpenMode::Fresh);
            self.pending_merge.replace(None);
        }
        self.refresh_queue();
        self.refresh_actions();
    }

    fn refuse_while_busy(self: &Rc<Self>) -> bool {
        if !self.is_idle() {
            let waiting = self.pending.borrow().len();
            if waiting > 0 {
                self.set_status(&format!("Still working ({waiting} queued)…"));
            } else {
                self.set_status("Still working on the last request…");
            }
        }
        !self.is_idle()
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

    /// For batch tools: they queue behind running jobs, so busyness is fine.
    fn require_document_queued(self: &Rc<Self>) -> bool {
        if !self.state.borrow().is_open() {
            self.set_status("Open a PDF first.");
            return false;
        }
        true
    }

    /// Show the dashboard grid, remembering what it covered.
    fn show_dashboard(self: &Rc<Self>) {
        if let Some(name) = self.ui.stack.visible_child_name() {
            if name != "dashboard" {
                self.previous_view.set(if name == "compare" {
                    "compare"
                } else {
                    "document"
                });
            }
        }
        self.ui.stack.set_visible_child_name("dashboard");
    }

    /// Show the open document.
    fn show_document(self: &Rc<Self>) {
        if self.state.borrow().is_open() {
            self.ui.stack.set_visible_child_name("document");
        }
    }

    /// Leave the dashboard for whatever it covered, if that is still there.
    fn leave_dashboard(self: &Rc<Self>) {
        let compare = self.compare_state.borrow();
        let compare_open = compare.left.is_open() || compare.right.is_open();
        drop(compare);
        if self.previous_view.get() == "compare" && compare_open {
            self.ui.stack.set_visible_child_name("compare");
        } else {
            self.show_document();
        }
    }

    fn toggle_dashboard(self: &Rc<Self>) {
        let showing = self.ui.stack.visible_child_name();
        if showing.as_deref() == Some("dashboard") {
            self.leave_dashboard();
        } else {
            self.show_dashboard();
        }
    }

    /// Rebuild the queue panel from the queue. Reads labels only: no IO.
    fn refresh_queue(self: &Rc<Self>) {
        while let Some(child) = self.ui.queue_list.first_child() {
            self.ui.queue_list.remove(&child);
        }

        let queue = self.queue.borrow();
        let waiting = queue.queued_count();
        let show = !queue.is_empty();
        self.ui
            .queue_label
            .set_label(&match (queue.running().is_some(), waiting) {
                (_, 0) if !show => "Queue".to_string(),
                (true, 0) => "Queue (1 running)".to_string(),
                (false, 0) => "Queue".to_string(),
                (true, waiting) => format!("Queue (1 running, {waiting} waiting)"),
                (false, waiting) => format!("Queue ({waiting} waiting)"),
            });
        self.ui.queue_scroll.set_visible(show);
        self.ui.queue_label.set_visible(show);
        self.ui.queue_cancel.set_visible(show);
        self.ui.queue_clear.set_visible(show);
        self.ui.queue_clear.set_sensitive(queue.has_finished());
        if let Some(action) = self.actions.borrow().get("cancel-waiting") {
            action.set_enabled(waiting > 0);
        }

        for job in queue.jobs() {
            let label = Label::builder()
                .label(job.summary())
                .xalign(0.0)
                .hexpand(true)
                .margin_start(8)
                .ellipsize(gtk4::pango::EllipsizeMode::Middle)
                .build();
            match job.status {
                JobStatus::Failed(_) => label.add_css_class("error"),
                JobStatus::Done(_) | JobStatus::Cancelled => label.add_css_class("dim-label"),
                _ => {}
            }
            if !job.detail.is_empty() {
                label.set_tooltip_text(Some(&job.detail));
            }

            let row = GtkBox::new(Orientation::Horizontal, 4);
            row.append(&label);
            if job.status == JobStatus::Queued {
                let cancel = Button::from_icon_name("window-close-symbolic");
                cancel.add_css_class("flat");
                cancel.set_tooltip_text(Some("Cancel this job"));
                let id = job.id;
                cancel.connect_clicked(clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |_| app.cancel_job(id)
                ));
                row.append(&cancel);
            }
            self.ui.queue_list.append(&row);
        }
    }

    /// Grey out whatever cannot be done right now. Immediate actions need an
    /// idle worker; batch tools only need their inputs, and queue while busy.
    /// What each tool needs comes from the registry in `tools.rs`.
    fn refresh_actions(self: &Rc<Self>) {
        let state = self.state.borrow();
        let idle = self.is_idle();
        let open = state.is_open();
        let pair = self.compare_state.borrow().both_open();
        let actions = self.actions.borrow();

        let enable = |name: &str, enabled: bool| {
            if let Some(action) = actions.get(name) {
                action.set_enabled(enabled);
            }
        };

        for name in DOCUMENT_ACTIONS {
            enable(name, idle && open);
        }
        for tool in TOOLS {
            enable(tool.id.action(), tool.enabled_for(open));
        }
        // The print preset is a second face of the Compress card.
        enable(
            "compress-print",
            TOOLS
                .iter()
                .any(|tool| tool.id.action() == "compress-screen" && tool.enabled_for(open)),
        );
        for (tool, button) in &self.ui.tool_buttons {
            button.set_tooltip_text(Some(&card_tooltip(tool, open)));
        }

        enable("open", idle);
        enable("save", idle && state.is_dirty());
        enable("undo", idle && state.can_undo());
        enable("redo", idle && state.can_redo());
        for name in ["zoom-in", "zoom-out", "zoom-fit"] {
            enable(name, open);
        }
        for name in COMPARE_OPEN_ACTIONS {
            enable(name, idle);
        }
        for name in COMPARE_PAIR_ACTIONS {
            enable(name, pair);
        }
        self.ui.compare_sync_button.set_sensitive(pair);
        self.ui.compare_diff_button.set_sensitive(pair);
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

/// Which side of the compare view a worker slot is.
fn side_of(slot: RenderSlot) -> ComparisonSide {
    match slot {
        RenderSlot::Primary => ComparisonSide::Left,
        RenderSlot::Secondary => ComparisonSide::Right,
    }
}

fn slot_of(side: ComparisonSide) -> RenderSlot {
    match side {
        ComparisonSide::Left => RenderSlot::Primary,
        ComparisonSide::Right => RenderSlot::Secondary,
    }
}

fn side_name(side: ComparisonSide) -> &'static str {
    match side {
        ComparisonSide::Left => "left",
        ComparisonSide::Right => "right",
    }
}

fn side_name_title(side: ComparisonSide) -> &'static str {
    match side {
        ComparisonSide::Left => "Left",
        ComparisonSide::Right => "Right",
    }
}

/// The zoom step above (positive) or below (negative) a scale, where 1.0 is
/// 100%. A scale between steps moves to the nearest step in that direction.
fn next_zoom(now: f32, direction: i32) -> f32 {
    if direction > 0 {
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
    }
}

/// The paths a multiple-file chooser returned.
fn paths_of(files: &gio::ListModel) -> Vec<PathBuf> {
    (0..files.n_items())
        .filter_map(|index| files.item(index).and_downcast::<gio::File>())
        .filter_map(|file| file.path())
        .collect()
}

/// The nine stamp positions, as the list shows them and as pdf-core names them.
const ANCHORS: [Anchor; 9] = [
    Anchor::TopLeft,
    Anchor::TopCenter,
    Anchor::TopRight,
    Anchor::MiddleLeft,
    Anchor::Center,
    Anchor::MiddleRight,
    Anchor::BottomLeft,
    Anchor::BottomCenter,
    Anchor::BottomRight,
];

const ANCHOR_NAMES: [&str; 9] = [
    "Top left",
    "Top centre",
    "Top right",
    "Middle left",
    "Centre",
    "Middle right",
    "Bottom left",
    "Bottom centre",
    "Bottom right",
];

const OFFICE_FORMATS: [OfficeFormat; 5] = [
    OfficeFormat::Docx,
    OfficeFormat::Odt,
    OfficeFormat::Rtf,
    OfficeFormat::Pptx,
    OfficeFormat::Xlsx,
];

const OFFICE_NAMES: [&str; 5] = [
    "Word document (.docx)",
    "OpenDocument text (.odt)",
    "Rich text (.rtf)",
    "PowerPoint presentation (.pptx)",
    "Excel spreadsheet (.xlsx)",
];

// ==================== FORMS ====================

/// The fields of a form dialog. Each call adds one row.
struct Fields {
    content: GtkBox,
}

impl Fields {
    /// A labelled row. Returns the row, so a form can show and hide it.
    fn add(&self, label: &str, widget: &impl IsA<gtk4::Widget>) -> gtk4::Widget {
        let row = GtkBox::new(Orientation::Horizontal, 12);
        row.append(
            &Label::builder()
                .label(label)
                .xalign(0.0)
                .width_chars(15)
                .build(),
        );
        widget.set_hexpand(true);
        row.append(widget);
        self.content.append(&row);
        row.upcast()
    }

    /// A widget across the whole width.
    fn wide(&self, widget: &impl IsA<gtk4::Widget>) {
        self.content.append(widget);
    }

    /// A short explanation in quiet type.
    fn note(&self, text: &str) -> gtk4::Widget {
        let label = Label::builder()
            .label(text)
            .wrap(true)
            .max_width_chars(48)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        self.content.append(&label);
        label.upcast()
    }
}

/// Show a modal form and call `done` with what it collected when the user
/// accepts. `build` adds the fields and returns the function that reads them
/// back; it can refuse with a message, which the form shows and stays open.
/// Closing the form any other way does nothing.
fn show_form<T: 'static>(
    parent: &ApplicationWindow,
    title: &str,
    accept: &str,
    build: impl FnOnce(&Fields) -> Box<dyn Fn() -> Result<T, String>>,
    done: impl FnOnce(T) + 'static,
) {
    let dialog = gtk4::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .destroy_with_parent(true)
        .default_width(440)
        .build();

    let content = GtkBox::new(Orientation::Vertical, 12);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let fields = Fields {
        content: GtkBox::new(Orientation::Vertical, 10),
    };
    let collect = build(&fields);

    let problem = Label::builder()
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(48)
        .css_classes(["error"])
        .visible(false)
        .build();

    let cancel = Button::with_label("Cancel");
    let ok = Button::builder()
        .label(accept)
        .css_classes(["suggested-action"])
        .build();
    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.set_halign(Align::End);
    buttons.set_margin_top(6);
    buttons.append(&cancel);
    buttons.append(&ok);

    content.append(&fields.content);
    content.append(&problem);
    content.append(&buttons);
    dialog.set_child(Some(&content));
    dialog.set_default_widget(Some(&ok));

    type Done<T> = Box<dyn FnOnce(T)>;
    let done: Rc<RefCell<Option<Done<T>>>> = Rc::new(RefCell::new(Some(Box::new(done))));

    ok.connect_clicked(clone!(
        #[weak]
        dialog,
        #[weak]
        problem,
        move |_| match collect() {
            Ok(value) => {
                dialog.close();
                let callback = done.borrow_mut().take();
                if let Some(callback) = callback {
                    callback(value);
                }
            }
            Err(message) => {
                problem.set_label(&message);
                problem.set_visible(true);
            }
        }
    ));
    cancel.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| dialog.close()
    ));

    // A message about an earlier attempt is stale as soon as the user changes
    // anything, so clear it on the next click or key. This runs before the
    // OK button's own handler, which shows a fresh message if one still applies.
    let clicks = gtk4::GestureClick::new();
    clicks.set_propagation_phase(gtk4::PropagationPhase::Capture);
    clicks.connect_released(clone!(
        #[weak]
        problem,
        move |_, _, _, _| problem.set_visible(false)
    ));
    content.add_controller(clicks);
    let typing = gtk4::EventControllerKey::new();
    typing.set_propagation_phase(gtk4::PropagationPhase::Capture);
    typing.connect_key_pressed(clone!(
        #[weak]
        problem,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, _, _, _| {
            problem.set_visible(false);
            glib::Propagation::Proceed
        }
    ));
    content.add_controller(typing);

    let escape = gtk4::EventControllerKey::new();
    escape.connect_key_pressed(clone!(
        #[weak]
        dialog,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                dialog.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    dialog.add_controller(escape);

    dialog.present();
}

fn text_entry(text: &str) -> gtk4::Entry {
    gtk4::Entry::builder()
        .text(text)
        .hexpand(true)
        .activates_default(true)
        .build()
}

fn password_entry() -> gtk4::PasswordEntry {
    gtk4::PasswordEntry::builder()
        .show_peek_icon(true)
        .activates_default(true)
        .hexpand(true)
        .build()
}

fn spin(value: f64, min: f64, max: f64, step: f64) -> gtk4::SpinButton {
    let button = gtk4::SpinButton::with_range(min, max, step);
    button.set_value(value);
    button.set_numeric(true);
    button
}

fn dropdown(items: &[&str]) -> DropDown {
    DropDown::from_strings(items)
}

/// A page-range field, set to every page.
fn pages_entry() -> gtk4::Entry {
    let entry = text_entry("all");
    entry.set_tooltip_text(Some("All pages, or a range such as 1-3,7,9-"));
    entry
}

/// Read a page-range field, checking it against the page count so a typo is
/// caught in the form rather than after the job starts.
fn pages_from(entry: &gtk4::Entry, page_count: usize) -> Result<PageRange, String> {
    let range = PageRange::parse(entry.text().as_str()).map_err(|error| error.to_string())?;
    range
        .resolve(page_count)
        .map_err(|error| error.to_string())?;
    Ok(range)
}

/// Call `change` now and again each time the dropdown's choice changes.
fn on_choice(dropdown: &DropDown, change: impl Fn(u32) + 'static) {
    change(dropdown.selected());
    dropdown.connect_selected_notify(move |dropdown| change(dropdown.selected()));
}

/// A small picture of a page with the part a margin crop keeps. `margins`
/// is read on every draw, so the picture follows the form.
fn margin_preview(
    paintable: Option<gdk::Paintable>,
    page_points: Option<(f64, f64)>,
    margins: impl Fn() -> MarginValues + 'static,
) -> gtk4::DrawingArea {
    // Fit the page into a box of this size, or show a sheet of paper when no
    // page has been drawn yet.
    const LONGEST: f64 = 200.0;
    let page = page_points.unwrap_or((595.0, 842.0));
    let scale = LONGEST / page.0.max(page.1);
    let (box_width, box_height) = (
        (page.0 * scale).round().max(40.0) as i32,
        (page.1 * scale).round().max(40.0) as i32,
    );

    let area = gtk4::DrawingArea::builder()
        .content_width(box_width)
        .content_height(box_height)
        .halign(Align::Center)
        .valign(Align::Start)
        .css_classes(["card"])
        .build();
    area.update_property(&[gtk4::accessible::Property::Label(
        "Preview of the part of the page that stays",
    )]);
    area.set_draw_func(move |_, cr, area_width, area_height| {
        let (w, h) = (f64::from(area_width), f64::from(area_height));

        // The page itself: white paper, then its picture when there is one.
        cr.set_source_rgb(1.0, 1.0, 1.0);
        let _ = cr.paint();
        if let Some(paintable) = &paintable {
            let snapshot = gtk4::Snapshot::new();
            paintable.snapshot(snapshot.upcast_ref::<gdk::Snapshot>(), w, h);
            if let Some(node) = snapshot.to_node() {
                node.draw(cr);
            }
        }

        let (left, top, right, bottom) = kept_fractions(&margins(), page);
        let (x0, y0, x1, y1) = (left * w, top * h, right * w, bottom * h);

        // Dim what will be hidden: four bands round the kept box.
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
        cr.rectangle(0.0, 0.0, w, y0);
        cr.rectangle(0.0, y1, w, h - y1);
        cr.rectangle(0.0, y0, x0, y1 - y0);
        cr.rectangle(x1, y0, w - x1, y1 - y0);
        let _ = cr.fill();

        cr.set_source_rgb(0.85, 0.2, 0.2);
        cr.set_line_width(1.5);
        cr.rectangle(
            x0 + 0.75,
            y0 + 0.75,
            (x1 - x0 - 1.5).max(0.0),
            (y1 - y0 - 1.5).max(0.0),
        );
        let _ = cr.stroke();
    });
    area
}

// ==================== SIGNATURE PAD ====================

/// What was drawn on the pad: strokes of points, top-left origin, in pixels.
type Strokes = Rc<RefCell<Vec<Vec<(f64, f64)>>>>;

/// A white pad to sign on with the mouse, a pen or a finger.
fn signature_pad(strokes: &Strokes) -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::builder()
        .content_width(420)
        .content_height(150)
        .hexpand(true)
        .css_classes(["card"])
        .build();
    area.update_property(&[gtk4::accessible::Property::Label("Signature pad")]);

    area.set_draw_func(clone!(
        #[strong]
        strokes,
        move |_, cr, width, height| {
            cr.set_source_rgb(1.0, 1.0, 1.0);
            let _ = cr.paint();

            // A faint line to sign along.
            let baseline = f64::from(height) * 0.75;
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.15);
            cr.set_line_width(1.0);
            cr.move_to(12.0, baseline);
            cr.line_to(f64::from(width) - 12.0, baseline);
            let _ = cr.stroke();

            cr.set_source_rgb(0.1, 0.1, 0.1);
            cr.set_line_width(2.5);
            cr.set_line_cap(gtk4::cairo::LineCap::Round);
            cr.set_line_join(gtk4::cairo::LineJoin::Round);
            for stroke in strokes.borrow().iter() {
                let mut points = stroke.iter();
                let Some((x, y)) = points.next() else {
                    continue;
                };
                cr.move_to(*x, *y);
                // A tap is a single point; a zero-length line with round caps
                // draws it as a dot.
                cr.line_to(*x, *y);
                for (x, y) in points {
                    cr.line_to(*x, *y);
                }
                let _ = cr.stroke();
            }
        }
    ));

    let drag = gtk4::GestureDrag::builder().button(0).build();
    drag.connect_drag_begin(clone!(
        #[strong]
        strokes,
        #[weak]
        area,
        move |_, x, y| {
            strokes.borrow_mut().push(vec![(x, y)]);
            area.queue_draw();
        }
    ));
    drag.connect_drag_update(clone!(
        #[strong]
        strokes,
        #[weak]
        area,
        move |gesture, dx, dy| {
            let Some((x, y)) = gesture.start_point() else {
                return;
            };
            if let Some(stroke) = strokes.borrow_mut().last_mut() {
                stroke.push((x + dx, y + dy));
            }
            area.queue_draw();
        }
    ));
    area.add_controller(drag);

    area
}

/// Turn what was drawn into strokes pdf-core can place: scaled so the longest
/// side is `size` points, with the origin at the bottom left and y pointing
/// up, as a PDF page expects. `None` when nothing worth stamping was drawn.
fn signature_strokes(raw: &[Vec<(f64, f64)>], size: f32) -> Option<Vec<Stroke>> {
    let points = || raw.iter().flatten();
    let (min_x, max_x) = points().fold((f64::MAX, f64::MIN), |(lo, hi), (x, _)| {
        (lo.min(*x), hi.max(*x))
    });
    let (min_y, max_y) = points().fold((f64::MAX, f64::MIN), |(lo, hi), (_, y)| {
        (lo.min(*y), hi.max(*y))
    });

    let longest = (max_x - min_x).max(max_y - min_y);
    // No points, or a mark too small to be a signature.
    if points().next().is_none() || longest < 4.0 {
        return None;
    }

    let scale = f64::from(size) / longest;
    Some(
        raw.iter()
            .filter(|stroke| !stroke.is_empty())
            .map(|stroke| {
                stroke
                    .iter()
                    .map(|(x, y)| Point {
                        x: ((x - min_x) * scale) as f32,
                        y: ((max_y - y) * scale) as f32,
                    })
                    .collect()
            })
            .collect(),
    )
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
        Task::CompareOpen => "Could not open the document for comparison",
        Task::Render => "Could not draw a page",
        Task::Save => "Could not save the document",
        Task::Compress => "Could not compress the document",
        Task::Ocr => "Could not make the document searchable",
        Task::Export => "Could not export the pages",
        Task::Merge => "Could not merge the files",
        Task::Split => "Could not split the document",
        Task::Stamp => "Could not add the watermark",
        Task::PageNumbers => "Could not add page numbers",
        Task::Sign => "Could not sign the document",
        Task::Crop => "Could not crop or resize the pages",
        Task::Redact => "Could not redact the document",
        Task::Flatten => "Could not flatten the document",
        Task::Encrypt => "Could not encrypt the document",
        Task::ExportText => "Could not extract text",
        Task::ExportHtml => "Could not export HTML",
        Task::FromImages => "Could not create PDF from images",
        Task::FromOffice => "Could not convert from office format",
        Task::ToOffice => "Could not convert to office format",
        Task::FromText => "Could not create PDF from text",
        Task::FromMarkdown => "Could not create PDF from Markdown",
        Task::Archive => "Could not make the archival or web copy",
        Task::Layout => "Could not lay out the pages",
        Task::Bookmark => "Could not add the bookmark",
        Task::FillForm => "Could not fill the form",
        Task::Create => "Could not create the document",
        Task::QrCode => "Could not create the QR code",
        Task::Inspect => "Could not check the document",
        Task::ExportMarkdown => "Could not export Markdown",
        Task::ExportSvg => "Could not export SVG",
        Task::Attach => "Could not attach the file",
        Task::Layers => "Could not change the layer",
        Task::PageLabels => "Could not set the page labels",
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
            if matches!(task, Task::Open | Task::Merge | Task::CompareOpen) =>
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
        let names: Vec<&str> = handlers().iter().map(|(name, _)| *name).collect();
        for (action, accels) in ACCELS {
            assert!(!accels.is_empty());
            match action.split_once('.') {
                Some(("win", name)) => assert!(names.contains(&name), "{action}"),
                Some(("app", "quit")) => {}
                _ => panic!("unexpected action {action}"),
            }
        }
    }

    #[test]
    fn every_tool_has_a_window_action() {
        let names: Vec<&str> = handlers().iter().map(|(name, _)| *name).collect();
        for tool in TOOLS {
            assert!(
                names.contains(&tool.id.action()),
                "{} has no handler",
                tool.title
            );
        }
        assert!(names.contains(&"compress-print"));
    }

    #[test]
    fn window_actions_are_named_once() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in handlers() {
            assert!(seen.insert(name), "{name} is registered twice");
        }
    }

    #[test]
    fn the_enable_lists_name_real_actions() {
        let names: Vec<&str> = handlers().iter().map(|(name, _)| *name).collect();
        for list in [DOCUMENT_ACTIONS, COMPARE_PAIR_ACTIONS, COMPARE_OPEN_ACTIONS] {
            for name in list {
                assert!(names.contains(name), "{name} has no handler");
            }
        }
    }

    #[test]
    fn zoom_steps_move_in_the_asked_direction() {
        assert_eq!(next_zoom(1.0, 1), 1.25);
        assert_eq!(next_zoom(1.0, -1), 0.75);
        // Between steps, the nearest one that way.
        assert_eq!(next_zoom(1.1, 1), 1.25);
        assert_eq!(next_zoom(1.1, -1), 1.0);
        // The ends hold.
        assert_eq!(next_zoom(4.0, 1), 4.0);
        assert_eq!(next_zoom(0.25, -1), 0.25);
    }

    #[test]
    fn the_signature_is_scaled_and_turned_right_way_up() {
        // A diagonal drawn from top-left down to bottom-right, 100 by 50.
        let drawn = vec![vec![(10.0, 10.0), (110.0, 60.0)]];
        let strokes = signature_strokes(&drawn, 200.0).unwrap();
        let points = &strokes[0];
        // Longest side (100) becomes 200 points, so the scale is 2.
        assert_eq!((points[0].x, points[0].y), (0.0, 100.0));
        assert_eq!((points[1].x, points[1].y), (200.0, 0.0));
    }

    #[test]
    fn an_empty_or_tiny_drawing_is_not_a_signature() {
        assert!(signature_strokes(&[], 100.0).is_none());
        assert!(signature_strokes(&[vec![(5.0, 5.0)]], 100.0).is_none());
        assert!(signature_strokes(&[vec![(5.0, 5.0), (6.0, 6.0)]], 100.0).is_none());
    }

    #[test]
    fn a_failed_compare_open_has_its_own_heading() {
        let (heading, _) = explain(
            Task::CompareOpen,
            &PdfError::NotFound(PathBuf::from("/a.pdf")),
        );
        assert_eq!(heading, "Could not open the document for comparison");
    }
}
