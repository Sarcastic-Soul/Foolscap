//! Foolscap, as a desktop application.
//!
//! Everything it can do, the `pdf-core` library already does; this is a window
//! onto it. No PDF logic lives here.

mod image;
mod state;
mod window;
mod worker;

use gtk4::prelude::*;
use gtk4::{gio, glib, Application};

const APP_ID: &str = "com.github.sarcastic_soul.Foolscap";

fn main() -> glib::ExitCode {
    init_tracing();

    let app = Application::builder()
        .application_id(APP_ID)
        // Accept a file on the command line, so that opening a PDF with
        // Foolscap from a file manager works.
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_activate(|app| {
        tracing::info!("activated with no document");
        window::build(app, None);
    });

    app.connect_open(|app, files, _hint| {
        let first = files.first().and_then(|file| file.path());
        tracing::info!(?first, "opening a document");
        window::build(app, first);
    });

    app.run()
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}
