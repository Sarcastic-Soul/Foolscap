//! SVG conversion tests. Only built with the `render` feature.

#![cfg(feature = "render")]

mod common;

use common::{build_text_page, build_with_image, ImagePlacement, Workspace};
use pdf_core::ops::svg::{plan, to_svg, SvgOptions};
use pdf_core::PageRange;
use std::fs;

#[test]
fn svg_output_for_text_heavy_page() {
    let workspace = Workspace::new();
    let path = workspace.write("text.pdf", build_text_page("Hello World SVG", 36.0));

    let out_dir = workspace.join("out");
    let options = SvgOptions {
        pages: PageRange::All,
        password: None,
    };

    let written = to_svg(&path, &out_dir, &options).unwrap();

    assert_eq!(written.len(), 1);
    assert!(written[0].exists());
    assert!(written[0].extension().unwrap() == "svg");

    let content = fs::read_to_string(&written[0]).unwrap();
    assert!(content.starts_with("<svg") || content.starts_with("<?xml"));
    assert!(content.contains("</svg>"));
}

#[test]
fn svg_output_for_image_heavy_page() {
    let workspace = Workspace::new();
    let path = workspace.write(
        "image.pdf",
        build_with_image(100, 100, ImagePlacement::drawn(200.0, 200.0)),
    );

    let out_dir = workspace.join("out2");
    let options = SvgOptions {
        pages: PageRange::All,
        password: None,
    };

    let written = to_svg(&path, &out_dir, &options).unwrap();

    assert_eq!(written.len(), 1);
    assert!(written[0].exists());
    assert!(written[0].extension().unwrap() == "svg");

    let content = fs::read_to_string(&written[0]).unwrap();
    assert!(content.starts_with("<svg") || content.starts_with("<?xml"));
    assert!(content.contains("</svg>"));
}

#[test]
fn svg_is_valid_xml() {
    let workspace = Workspace::new();
    let path = workspace.document("in.pdf", 1, "Test");

    let out_dir = workspace.join("out3");
    let options = SvgOptions {
        pages: PageRange::All,
        password: None,
    };

    let written = to_svg(&path, &out_dir, &options).unwrap();

    let content = fs::read_to_string(&written[0]).unwrap();
    // Basic XML validation - should parse without error
    let _ = roxmltree::Document::parse(&content).expect("SVG should be valid XML");
}

#[test]
fn svg_page_range_selection() {
    let workspace = Workspace::new();
    let path = workspace.document("multi.pdf", 3, "Page");

    let out_dir = workspace.join("out4");
    let options = SvgOptions {
        pages: PageRange::parse("1-2").unwrap(),
        password: None,
    };

    let written = to_svg(&path, &out_dir, &options).unwrap();

    assert_eq!(written.len(), 2);
    assert!(written[0].exists());
    assert!(written[1].exists());
    assert!(written[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("-1"));
    assert!(written[1]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("-2"));
}

#[test]
fn svg_plan_matches_output() {
    let workspace = Workspace::new();
    let path = workspace.document("multi.pdf", 3, "Page");

    let out_dir = workspace.join("out5");
    let options = SvgOptions {
        pages: PageRange::parse("2").unwrap(),
        password: None,
    };

    let planned = plan(&path, &out_dir, &options).unwrap();
    let written = to_svg(&path, &out_dir, &options).unwrap();

    assert_eq!(planned.len(), 1);
    assert_eq!(written.len(), 1);
    assert_eq!(planned[0], written[0]);
}
