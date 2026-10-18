//! The Foolscap command-line interface.
//!
//! Split into a library so that the man page and shell completions can be
//! generated from the same `clap` definition the binary uses, rather than a
//! copy of it that would quietly drift.

pub mod commands;
pub mod output;

use anyhow::Result;
use clap::{Parser, Subcommand};

use commands::{
    arrange, compress, decrypt, encrypt, flatten, info, merge, meta, optimize, repair, rotate,
    split, stamp, strip,
};
use pdf_core::PdfError;

#[derive(Debug, Parser)]
#[command(
    name = "foolscap",
    version,
    about = "A Linux-first PDF toolkit",
    long_about = None,
)]
pub struct Cli {
    /// Increase log verbosity. Repeat for more detail.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Overwrite output files that already exist.
    #[arg(short, long, global = true)]
    force: bool,

    /// Password for encrypted input documents. Only used for files that need
    /// one; files with an empty user password open without it.
    #[arg(
        long,
        global = true,
        env = "FOOLSCAP_PASSWORD",
        hide_env_values = true,
        value_name = "PASSWORD"
    )]
    password: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Concatenate documents into one.
    Merge(merge::Args),
    /// Extract pages, or burst a document into pieces.
    Split(split::Args),
    /// Rotate pages in place.
    Rotate(rotate::Args),
    /// Remove pages.
    Delete(arrange::DeleteArgs),
    /// Put pages in a different order.
    Reorder(arrange::ReorderArgs),
    /// Show page count, size, and metadata.
    Info(info::Args),
    /// Read or edit the document information dictionary.
    Meta(meta::Args),
    /// Write an unencrypted copy of a password-protected document.
    Decrypt(decrypt::Args),
    /// Protect a document with a password (AES-256).
    Encrypt(encrypt::Args),
    /// Repair a damaged or corrupt PDF.
    Repair(repair::Args),
    /// Remove identifying metadata: Info entries, XMP, and the document ID.
    Strip(strip::Args),
    /// Bake form fields and annotations into the page content.
    Flatten(flatten::Args),
    /// Losslessly shrink a document.
    Optimize(optimize::Args),
    /// Shrink a document by resampling its images. Lossy.
    Compress(compress::Args),
    /// Crop pages to a rectangle, resize them, or restore the full page.
    Crop(commands::crop::Args),
    /// Recolour embedded images. Vector text is left alone.
    Filter(commands::filter::Args),
    /// Cover an area or a found string so it is neither visible nor searchable.
    Redact(commands::redact::Args),
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
    /// Typeset a plain-text file as a PDF.
    #[cfg(feature = "convert")]
    FromText(commands::convert::FromTextArgs),
    /// Typeset a Markdown file as a PDF.
    #[cfg(feature = "convert")]
    FromMarkdown(commands::convert::FromMarkdownArgs),
    /// Convert an HTML file into a PDF.
    #[cfg(feature = "convert")]
    FromHtml(commands::convert::FromHtmlArgs),
    /// Convert a PDF into a single HTML file.
    #[cfg(all(feature = "convert", feature = "render"))]
    ToHtml(commands::convert::ToHtmlArgs),
    /// Rasterise pages into one zip archive of images.
    #[cfg(all(feature = "convert", feature = "render"))]
    ToZip(commands::to_images::ZipArgs),
    /// Add an invisible text layer so a scan becomes searchable.
    #[cfg(feature = "ocr")]
    Ocr(commands::ocr::OcrArgs),
    /// Read out the text a document already carries. No recognition.
    #[cfg(feature = "ocr")]
    ExtractText(commands::ocr::ExtractTextArgs),
    /// Pull every embedded image out of a PDF.
    ExtractImages(commands::extract_images::Args),
    /// Draw text or an image over or under pages.
    Stamp(stamp::StampArgs),
    /// Draw a large translucent mark, diagonally by default.
    Watermark(stamp::WatermarkArgs),
    /// Draw running page numbers.
    PageNumbers(stamp::PageNumbersArgs),
    /// Draw a running header and/or footer.
    HeaderFooter(stamp::HeaderFooterArgs),
    /// Show which optional features this build supports.
    Capabilities,
}

pub fn run(cli: Cli) -> Result<()> {
    init_tracing(cli.verbose);

    let force = cli.force;
    let password = cli.password.as_deref();

    let outcome = match cli.command {
        Command::Merge(args) => merge::run(args, force, password),
        Command::Split(args) => split::run(args, force, password),
        Command::Rotate(args) => rotate::run(args, force, password),
        Command::Delete(args) => arrange::delete(args, force, password),
        Command::Reorder(args) => arrange::reorder(args, force, password),
        Command::Info(args) => info::run(args, password),
        Command::Meta(args) => meta::run(args, force, password),
        Command::Decrypt(args) => decrypt::run(args, force, password),
        Command::Encrypt(args) => encrypt::run(args, force, password),
        Command::Repair(args) => repair::run(args, force, password),
        Command::Strip(args) => strip::run(args, force, password),
        Command::Flatten(args) => flatten::run(args, force, password),
        Command::Optimize(args) => optimize::run(args, force, password),
        Command::Compress(args) => compress::run(args, force, password),
        Command::Crop(args) => commands::crop::run(args, force, password),
        Command::Filter(args) => commands::filter::run(args, force, password),
        Command::Redact(args) => commands::redact::run(args, force, password),
        #[cfg(feature = "render")]
        Command::Render(args) => commands::render::render(args, force, password),
        #[cfg(feature = "render")]
        Command::Thumbnail(args) => commands::render::thumbnail(args, force, password),
        #[cfg(feature = "convert")]
        Command::FromImages(args) => commands::convert::from_images(args, force),
        #[cfg(all(feature = "convert", feature = "render"))]
        Command::ToImages(args) => commands::to_images::run(args, force, password),
        #[cfg(feature = "convert")]
        Command::FromOffice(args) => commands::convert::from_office(args, force),
        #[cfg(feature = "convert")]
        Command::ToOffice(args) => commands::convert::to_office(args, force, password),
        #[cfg(feature = "convert")]
        Command::FromText(args) => commands::convert::from_text(args, force),
        #[cfg(feature = "convert")]
        Command::FromMarkdown(args) => commands::convert::from_markdown(args, force),
        #[cfg(feature = "convert")]
        Command::FromHtml(args) => commands::convert::from_html(args, force),
        #[cfg(all(feature = "convert", feature = "render"))]
        Command::ToHtml(args) => commands::convert::to_html(args, force, password),
        #[cfg(all(feature = "convert", feature = "render"))]
        Command::ToZip(args) => commands::to_images::zip(args, force, password),
        #[cfg(feature = "ocr")]
        Command::Ocr(args) => commands::ocr::run(args, force, password),
        #[cfg(feature = "ocr")]
        Command::ExtractText(args) => commands::ocr::extract_text(args, force, password),
        Command::ExtractImages(args) => commands::extract_images::run(args, force, password),
        Command::Stamp(args) => commands::stamp::stamp(args, force, password),
        Command::Watermark(args) => commands::stamp::watermark(args, force, password),
        Command::PageNumbers(args) => commands::stamp::page_numbers(args, force, password),
        Command::HeaderFooter(args) => commands::stamp::header_footer(args, force, password),
        Command::Capabilities => {
            output::print_capabilities();
            Ok(())
        }
    };

    outcome.map_err(explain)
}

/// Add the command-line remedy to errors that have one. `pdf-core` cannot name
/// a flag: the GUI asks for a password in a dialog instead.
fn explain(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<PdfError>() {
        Some(PdfError::Encrypted(path)) => anyhow::anyhow!(
            "{} is encrypted; pass --password (or set FOOLSCAP_PASSWORD)",
            path.display()
        ),
        Some(PdfError::WrongPassword(path)) => {
            anyhow::anyhow!("wrong password for {}", path.display())
        }
        _ => error,
    }
}

fn init_tracing(verbosity: u8) {
    let level = match verbosity {
        // lopdf warns on every encrypted file it loads, before pdf-core has
        // tried the password, and pdf-core reports the outcome as a typed
        // error anyway. Its warnings come back with -v.
        0 => "warn,lopdf=error",
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
