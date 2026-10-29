//! Generate QR-code documents.
//!
//! The code itself comes from `qrcodegen`; the page around it is built with
//! `lopdf` alone, exactly as the stamp engine builds its content: a stream of
//! `x y w h re f` rectangle fills, one per dark module, so the symbol is
//! vector art and stays crisp at any scale. No raster image is embedded.

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Object, Stream};

use crate::document::Document;
use crate::error::{PdfError, Result};

/// Millimetres to PDF points.
const MM_TO_PT: f64 = 72.0 / 25.4;
/// Quiet-zone width in modules, on every side of the symbol. Four is the
/// minimum the QR specification requires for reliable scanning.
const QUIET_ZONE: f64 = 4.0;
/// Name of the label font in the page resources.
const FONT_NAME: &str = "F1";

/// Error-correction level of the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrEcc {
    Low,
    Medium,
    Quartile,
    High,
}

impl QrEcc {
    fn to_qrcodegen(self) -> qrcodegen::QrCodeEcc {
        match self {
            QrEcc::Low => qrcodegen::QrCodeEcc::Low,
            QrEcc::Medium => qrcodegen::QrCodeEcc::Medium,
            QrEcc::Quartile => qrcodegen::QrCodeEcc::Quartile,
            QrEcc::High => qrcodegen::QrCodeEcc::High,
        }
    }
}

/// Options for [`qr_code`].
#[derive(Debug, Clone)]
pub struct QrOptions {
    /// Edge length of the code in millimetres.
    pub size_mm: f64,
    pub ecc: QrEcc,
    /// Label drawn beneath the code.
    pub label: Option<String>,
}

/// Build a one-page document carrying a QR code for `text`.
///
/// The page is a square of `size_mm` by `size_mm` millimetres holding the
/// symbol with its four-module quiet zone; when a label is given, a strip
/// tall enough for it is added beneath, and the label is centred there in the
/// unembedded Base14 font Helvetica.
pub fn qr_code(text: &str, options: &QrOptions) -> Result<Document> {
    if !options.size_mm.is_finite() || options.size_mm <= 0.0 {
        return Err(PdfError::InvalidQrCode(format!(
            "size must be a positive number of millimetres, got {}",
            options.size_mm
        )));
    }

    let code = qrcodegen::QrCode::encode_text(text, options.ecc.to_qrcodegen())
        .map_err(|source| PdfError::InvalidQrCode(source.to_string()))?;
    let dark = dark_modules(&code);

    let size_pt = options.size_mm * MM_TO_PT;
    let modules = code.size() as f64;
    let module_pt = size_pt / (modules + 2.0 * QUIET_ZONE);

    // Without a label the page is the square of the code. With one, a strip
    // for the text is added beneath: the font size tracks the page, clamped
    // so tiny labels stay legible and huge ones do not dominate the page.
    let (label, font_size, strip_pt) = match &options.label {
        Some(label) if !label.trim().is_empty() => {
            let font_size = (size_pt * 0.12).clamp(6.0, 20.0);
            // Room for ascender and descender, plus breathing room above.
            let strip = font_size * 1.8;
            (Some(label.trim()), font_size, strip)
        }
        _ => (None, 0.0, 0.0),
    };

    let page_w = size_pt;
    let page_h = size_pt + strip_pt;

    // The symbol sits in the top square of the page; PDF coordinates run from
    // the bottom-left while qrcodegen numbers rows from the top.
    let code_x = QUIET_ZONE * module_pt;
    let code_y = strip_pt + QUIET_ZONE * module_pt;

    // lopdf carries reals as f32; every coordinate below is well inside its
    // seven significant digits.
    let pt = |value: f64| Object::Real(value as f32);

    let mut operations = vec![
        // An explicit white background, so the page reads as paper even in a
        // viewer that fills unmarked areas with something else.
        Operation::new("rg", vec![pt(1.0), pt(1.0), pt(1.0)]),
        Operation::new("re", vec![pt(0.0), pt(0.0), pt(page_w), pt(page_h)]),
        Operation::new("f", vec![]),
        Operation::new("rg", vec![pt(0.0), pt(0.0), pt(0.0)]),
    ];

    for (x, y) in dark {
        let bottom = code_y + (modules - 1.0 - y as f64) * module_pt;
        operations.push(Operation::new(
            "re",
            vec![
                pt(code_x + x as f64 * module_pt),
                pt(bottom),
                pt(module_pt),
                pt(module_pt),
            ],
        ));
        operations.push(Operation::new("f", vec![]));
    }

    if let Some(label) = label {
        // Centre the label under the code, following the stamp engine's
        // BT / Tf / Td / Tj / ET pattern with a Base14 font.
        let width = super::stamp::estimate_text_width(label, font_size as f32);
        let x = ((page_w - width as f64) / 2.0).max(0.0);
        let y = (strip_pt - font_size) / 2.0;

        operations.push(Operation::new("BT", vec![]));
        operations.push(Operation::new(
            "Tf",
            vec![Object::Name(FONT_NAME.as_bytes().to_vec()), pt(font_size)],
        ));
        operations.push(Operation::new("rg", vec![pt(0.0), pt(0.0), pt(0.0)]));
        operations.push(Operation::new("Td", vec![pt(x), pt(y)]));
        operations.push(Operation::new(
            "Tj",
            vec![Object::string_literal(label.as_bytes().to_vec())],
        ));
        operations.push(Operation::new("ET", vec![]));
    }

    let content = Content { operations }.encode().map_err(|source| {
        PdfError::Internal(format!("could not encode the QR content: {source}"))
    })?;

    let mut doc = lopdf::Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let content_id = doc.add_object(Stream::new(Dictionary::new(), content));

    let mut resources = Dictionary::new();
    if label.is_some() {
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        resources.set("Font", dictionary! { FONT_NAME => font_id });
    }

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources,
        "MediaBox" => vec![pt(0.0), pt(0.0), pt(page_w), pt(page_h)],
    });

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => 1,
            "Kids" => vec![page_id.into()],
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    Ok(Document::from_lopdf(doc, None))
}

/// The dark modules of `code` as `(x, y)` pairs, with `y` counted from the
/// top row exactly as `qrcodegen` numbers them.
fn dark_modules(code: &qrcodegen::QrCode) -> Vec<(usize, usize)> {
    let size = code.size();
    let mut dark = Vec::new();
    for y in 0..size {
        for x in 0..size {
            if code.get_module(x, y) {
                dark.push((x as usize, y as usize));
            }
        }
    }
    dark
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_matrix_matches_qrcodegen() {
        let code = qrcodegen::QrCode::encode_text("HELLO WORLD 123", qrcodegen::QrCodeEcc::Medium)
            .expect("a short string always encodes");

        let dark = dark_modules(&code);

        // The same grid, walked directly, must produce the same set.
        let size = code.size();
        let mut expected = Vec::new();
        for y in 0..size {
            for x in 0..size {
                if code.get_module(x, y) {
                    expected.push((x as usize, y as usize));
                }
            }
        }

        assert_eq!(dark, expected);
        assert!(!dark.is_empty());
        // A known input pins the symbol size: 15 alphanumeric characters at
        // medium correction fit a version 1 code, 21 modules on a side.
        assert_eq!(size, 21, "the known input encodes as a version 1 code");
    }

    #[test]
    fn oversized_input_is_rejected() {
        let options = QrOptions {
            size_mm: 50.0,
            ecc: QrEcc::High,
            label: None,
        };

        let error = qr_code(&"x".repeat(4000), &options).unwrap_err();
        assert!(matches!(error, PdfError::InvalidQrCode(_)), "got {error:?}");
    }

    #[test]
    fn degenerate_sizes_are_rejected() {
        for size_mm in [0.0, -10.0, f64::NAN, f64::INFINITY] {
            let options = QrOptions {
                size_mm,
                ecc: QrEcc::Low,
                label: None,
            };
            let error = qr_code("hi", &options).unwrap_err();
            assert!(
                matches!(error, PdfError::InvalidQrCode(_)),
                "{size_mm} should have been rejected, got {error:?}"
            );
        }
    }
}
