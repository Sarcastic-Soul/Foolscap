//! Foolscap command-line interface.
//!
//! This layer parses arguments, installs a `tracing` subscriber, calls into
//! `pdf-core`, and formats the result. It holds no PDF logic of its own.

mod commands;
mod output;

use anyhow::Result;
use clap::{Parser, Subcommand};

use commands::{compress, info, merge, meta, optimize, rotate, split};

#[derive(Debug, Parser)]
#[command(
    name = "foolscap",
    version,
    about = "A Linux-first PDF toolkit",
    long_about = None,
)]
struct Cli {
    /// Increase log verbosity. Repeat for more detail.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Overwrite output files that already exist.
    #[arg(short, long, global = true)]
    force: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Concatenate documents into one.
    Merge(merge::Args),
    /// Extract pages, or burst a document into pieces.
    Split(split::Args),
    /// Rotate pages in place.
    Rotate(rotate::Args),
    /// Show page count, size, and metadata.
    Info(info::Args),
    /// Read or edit the document information dictionary.
    Meta(meta::Args),
    /// Losslessly shrink a document.
    Optimize(optimize::Args),
    /// Shrink a document by resampling its images. Lossy.
    Compress(compress::Args),
    /// Rasterise pages to PNG.
    #[cfg(feature = "render")]
    Render(commands::render::RenderArgs),
    /// Write a single small preview image of one page.
    #[cfg(feature = "render")]
    Thumbnail(commands::render::ThumbnailArgs),
    /// Bind images into a PDF, one page per image.
    #[cfg(feature = "convert")]
    FromImages(commands::convert::FromImagesArgs),
    /// Rasterise every page to an image file.
    #[cfg(all(feature = "convert", feature = "render"))]
    ToImages(commands::to_images::Args),
    /// Convert a document LibreOffice can open into a PDF.
    #[cfg(feature = "convert")]
    FromOffice(commands::convert::FromOfficeArgs),
    /// Convert a PDF into an editable document. Approximate.
    #[cfg(feature = "convert")]
    ToOffice(commands::convert::ToOfficeArgs),
    /// Add an invisible text layer so a scan becomes searchable.
    #[cfg(feature = "ocr")]
    Ocr(commands::ocr::OcrArgs),
    /// Read out the text a document already carries. No recognition.
    #[cfg(feature = "ocr")]
    ExtractText(commands::ocr::ExtractTextArgs),
    /// Show which optional features this build supports.
    Capabilities,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let force = cli.force;

    match cli.command {
        Command::Merge(args) => merge::run(args, force),
        Command::Split(args) => split::run(args, force),
        Command::Rotate(args) => rotate::run(args, force),
        Command::Info(args) => info::run(args),
        Command::Meta(args) => meta::run(args, force),
        Command::Optimize(args) => optimize::run(args, force),
        Command::Compress(args) => compress::run(args, force),
        #[cfg(feature = "render")]
        Command::Render(args) => commands::render::render(args, force),
        #[cfg(feature = "render")]
        Command::Thumbnail(args) => commands::render::thumbnail(args, force),
        #[cfg(feature = "convert")]
        Command::FromImages(args) => commands::convert::from_images(args, force),
        #[cfg(all(feature = "convert", feature = "render"))]
        Command::ToImages(args) => commands::to_images::run(args, force),
        #[cfg(feature = "convert")]
        Command::FromOffice(args) => commands::convert::from_office(args, force),
        #[cfg(feature = "convert")]
        Command::ToOffice(args) => commands::convert::to_office(args, force),
        #[cfg(feature = "ocr")]
        Command::Ocr(args) => commands::ocr::run(args, force),
        #[cfg(feature = "ocr")]
        Command::ExtractText(args) => commands::ocr::extract_text(args, force),
        Command::Capabilities => {
            output::print_capabilities();
            Ok(())
        }
    }
}

fn init_tracing(verbosity: u8) {
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Catches conflicts between global and subcommand flags — a duplicate
    /// short option only panics when that subcommand is actually invoked, so
    /// without this it escapes into a release.
    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }
}
