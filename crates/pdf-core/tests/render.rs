//! Rendering tests. Only built with the `render` feature.

#![cfg(feature = "render")]

mod common;

use common::{Attributes, Workspace, A4, LETTER};
use pdf_core::render::{PageRenderer, Scale, POINTS_PER_INCH};
use pdf_core::PdfError;

#[test]
fn a_page_renders_at_the_size_its_dpi_implies() {
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 1, "Page");

    let renderer = PageRenderer::open(&path).unwrap();
    let page = renderer.render(0, Scale::Dpi(72.0)).unwrap();

    assert_eq!(page.width, A4.0 as u32);
    assert_eq!(page.height, A4.1 as u32);
    // Rendered without an alpha channel, so a page reads as paper.
    assert_eq!(page.channels, 3);
    assert_eq!(page.pixels.len(), page.stride() * page.height as usize);
}

#[test]
fn doubling_the_dpi_doubles_the_pixels() {
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 1, "Page");
    let renderer = PageRenderer::open(&path).unwrap();

    let low = renderer.render(0, Scale::Dpi(72.0)).unwrap();
    let high = renderer.render(0, Scale::Dpi(144.0)).unwrap();

    assert_eq!(high.width, low.width * 2);
    assert_eq!(high.height, low.height * 2);
}

#[test]
fn a_thumbnail_fits_inside_the_box_it_was_given() {
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 1, "Page");
    let renderer = PageRenderer::open(&path).unwrap();

    let thumb = renderer.thumbnail(0, 256).unwrap();

    assert!(thumb.width <= 256 && thumb.height <= 256);
    assert!(thumb.width.max(thumb.height) >= 255, "should fill the box");
}

#[test]
fn a_rendered_page_is_not_blank() {
    // The fixture draws text, so some pixels must differ from the background.
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 1, "Page");
    let renderer = PageRenderer::open(&path).unwrap();

    let page = renderer.render(0, Scale::Dpi(72.0)).unwrap();

    // The background is white; the text is not.
    assert!(
        page.pixels.iter().any(|&sample| sample != 255),
        "every pixel was white, so nothing was drawn"
    );
}

#[test]
fn the_second_request_comes_from_the_cache() {
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 2, "Page");
    let renderer = PageRenderer::open(&path).unwrap();

    renderer.render(0, Scale::Dpi(72.0)).unwrap();
    renderer.render(0, Scale::Dpi(72.0)).unwrap();
    renderer.render(1, Scale::Dpi(72.0)).unwrap();

    let stats = renderer.cache_stats();
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 2);
}

#[test]
fn a_different_scale_is_a_different_cache_entry() {
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 1, "Page");
    let renderer = PageRenderer::open(&path).unwrap();

    renderer.render(0, Scale::Dpi(72.0)).unwrap();
    renderer.render(0, Scale::Dpi(150.0)).unwrap();

    assert_eq!(renderer.cache_stats().misses, 2);
    assert_eq!(renderer.cache_stats().entries, 2);
}

#[test]
fn page_size_reports_points_before_scaling() {
    let workspace = Workspace::new();
    let path = workspace.write(
        "letter.pdf",
        common::build(1, "Page", LETTER, Attributes::Inherited),
    );

    let renderer = PageRenderer::open(&path).unwrap();
    let (width, height) = renderer.page_size(0).unwrap();

    assert!((width - LETTER.0 as f32).abs() < 0.5);
    assert!((height - LETTER.1 as f32).abs() < 0.5);
}

#[test]
fn rendering_past_the_last_page_is_an_error() {
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 2, "Page");
    let renderer = PageRenderer::open(&path).unwrap();

    let error = renderer.render(5, Scale::Dpi(72.0)).unwrap_err();
    assert!(
        matches!(
            error,
            PdfError::PageOutOfRange {
                requested: 6,
                total: 2
            }
        ),
        "got {error:?}"
    );
}

#[test]
fn opening_a_missing_file_says_so() {
    let workspace = Workspace::new();
    let error = PageRenderer::open(workspace.join("absent.pdf")).unwrap_err();
    assert!(matches!(error, PdfError::NotFound(_)), "got {error:?}");
}

#[test]
fn inherited_page_geometry_is_honoured_when_rendering() {
    let workspace = Workspace::new();
    let path = workspace.write(
        "letter.pdf",
        common::build(1, "Page", LETTER, Attributes::Inherited),
    );

    let renderer = PageRenderer::open(&path).unwrap();
    let page = renderer.render(0, Scale::Dpi(POINTS_PER_INCH)).unwrap();

    assert_eq!(page.width, LETTER.0 as u32);
    assert_eq!(page.height, LETTER.1 as u32);
}

#[test]
fn unmarked_areas_come_back_white() {
    // A page is paper. Rendered with an alpha channel, blank areas would be
    // transparent and show whatever is behind them.
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 1, "Page");
    let renderer = PageRenderer::open(&path).unwrap();

    let page = renderer.render(0, Scale::Dpi(72.0)).unwrap();

    // The bottom right corner of the fixture is empty.
    let last = page.pixels.len() - page.channels as usize;
    assert!(
        page.pixels[last..].iter().all(|&sample| sample == 255),
        "the corner should be white, got {:?}",
        &page.pixels[last..]
    );
}
