//! SVG conversion tests. Only built with the `render` feature.

#![cfg(feature = "render")]

mod common;

use common::{build_text_page, build_with_image, Attributes, ImagePlacement, Workspace, A4};
use pdf_core::ops::svg::{plan, to_svg, FontEmbedding, SvgOptions};
use pdf_core::PageRange;
use std::fs;

#[test]
fn svg_output_for_text_heavy_page() {
    let workspace = Workspace::new();
    let path = workspace.write("text.pdf", build_text_page("Hello World SVG", 36.0));

    let out_dir = workspace.join("out");
    let options = SvgOptions {
        pages: PageRange::All,
        font_embedding: FontEmbedding::Subset,
        text_as_paths: false,
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
        font_embedding: FontEmbedding::Subset,
        text_as_paths: false,
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
        font_embedding: FontEmbedding::Subset,
        text_as_paths: false,
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
        font_embedding: FontEmbedding::Subset,
        text_as_paths: false,
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
        font_embedding: FontEmbedding::Subset,
        text_as_paths: false,
        password: None,
    };

    let planned = plan(&path, &out_dir, &options).unwrap();
    let written = to_svg(&path, &out_dir, &options).unwrap();

    assert_eq!(planned.len(), 1);
    assert_eq!(written.len(), 1);
    assert_eq!(planned[0], written[0]);
}

#[test]
fn svg_font_embedding_options() {
    let workspace = Workspace::new();
    let path = workspace.write("text.pdf", build_text_page("Hello World SVG", 36.0));

    let out_dir = workspace.join("out6");

    // Test Reference embedding
    let options_ref = SvgOptions {
        pages: PageRange::All,
        font_embedding: FontEmbedding::Reference,
        text_as_paths: false,
        password: None,
    };
    let written_ref = to_svg(&path, &out_dir, &options_ref).unwrap();
    assert!(written_ref[0].exists());

    // Test Subset embedding (default)
    let options_sub = SvgOptions {
        pages: PageRange::All,
        font_embedding: FontEmbedding::Subset,
        text_as_paths: false,
        password: None,
    };
    let written_sub = to_svg(&path, &out_dir, &options_sub).unwrap();
    assert!(written_sub[0].exists());

    // Test None embedding
    let options_none = SvgOptions {
        pages: PageRange::All,
        font_embedding: FontEmbedding::None,
        text_as_paths: false,
        password: None,
    };
    let written_none = to_svg(&path, &out_dir, &options_none).unwrap();
    assert!(written_none[0].exists());
}

#[test]
fn svg_text_as_paths_option() {
    let workspace = Workspace::new();
    let path = workspace.write("text.pdf", build_text_page("Hello World SVG", 36.0));

    let out_dir = workspace.join("out7");
    let options = SvgOptions {
        pages: PageRange::All,
        font_embedding: FontEmbedding::Subset,
        text_as_paths: true,
        password: None,
    };

    let written = to_svg(&path, &out_dir, &options).unwrap();
    assert_eq!(written.len(), 1);
    assert!(written[0].exists());

    let content = fs::read_to_string(&written[0]).unwrap();
    assert!(content.starts_with("<svg") || content.starts_with("<?xml"));
    assert!(content.contains("</svg>"));
}
