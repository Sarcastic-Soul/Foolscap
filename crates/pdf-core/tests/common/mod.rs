//! Synthetic documents for the integration tests.
//!
//! Fixtures are generated rather than committed as binaries. Page geometry and
//! attribute placement are exactly the things these tests need to vary, and a
//! handful of checked-in files cannot cover the combinations — in particular
//! whether `/MediaBox` and `/Resources` sit on the page or are inherited from
//! the page tree, which is where reassembly bugs hide.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use tempfile::TempDir;

/// Where a page's inheritable attributes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attributes {
    /// `/MediaBox` and `/Resources` on each page. The easy case.
    OnPage,
    /// Both on the `/Pages` node, inherited by every page. The case that breaks
    /// naive merges.
    Inherited,
}

/// A4 in PDF points.
pub const A4: (i64, i64) = (595, 842);
/// US Letter in PDF points, so that differently-sized inputs can be told apart.
pub const LETTER: (i64, i64) = (612, 792);

/// Build a document whose pages are labelled `{label} 1`, `{label} 2`, and so
/// on, so that page identity survives into the content stream and can be
/// asserted on after a reassembly.
pub fn build(page_count: usize, label: &str, size: (i64, i64), attributes: Attributes) -> Document {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let media_box: Vec<Object> = vec![0.into(), 0.into(), size.0.into(), size.1.into()];

    let mut kids = Vec::with_capacity(page_count);
    for number in 1..=page_count {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 36.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal(format!("{label} {number}"))],
                ),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));

        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        };

        if attributes == Attributes::OnPage {
            page.set("Resources", Object::Reference(resources_id));
            page.set("MediaBox", Object::Array(media_box.clone()));
        }

        kids.push(Object::Reference(doc.add_object(page)));
    }

    let mut pages = dictionary! {
        "Type" => "Pages",
        "Count" => page_count as i64,
        "Kids" => Object::Array(kids),
    };

    if attributes == Attributes::Inherited {
        pages.set("Resources", Object::Reference(resources_id));
        pages.set("MediaBox", Object::Array(media_box));
    }

    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    doc
}

/// A scratch directory that cleans itself up, plus helpers for putting
/// documents in it.
pub struct Workspace {
    dir: TempDir,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new().expect("could not create a temporary directory"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Path inside the workspace, without creating anything.
    pub fn join(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Write a generated document into the workspace and return its path.
    pub fn write(&self, name: &str, mut doc: Document) -> PathBuf {
        let path = self.join(name);
        doc.save(&path).expect("could not write fixture");
        path
    }

    /// Shorthand for the common case: an N-page A4 document with per-page
    /// attributes.
    pub fn document(&self, name: &str, page_count: usize, label: &str) -> PathBuf {
        self.write(name, build(page_count, label, A4, Attributes::OnPage))
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

/// Read back the text each page draws, in page order.
///
/// This is how the tests assert that pages kept their identity and order
/// through an operation.
pub fn page_labels(path: &Path) -> Vec<String> {
    let doc = Document::load(path).expect("could not reload document");

    doc.get_pages()
        .into_values()
        .map(|page_id| {
            let content = doc
                .get_page_content(page_id)
                .expect("page has no content stream");
            let decoded = Content::decode(&content).expect("could not decode content stream");

            decoded
                .operations
                .iter()
                .filter(|operation| operation.operator == "Tj")
                .filter_map(|operation| operation.operands.first())
                .filter_map(|operand| match operand {
                    Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Read each page's effective `/MediaBox`, following inheritance.
pub fn page_media_boxes(path: &Path) -> Vec<Vec<i64>> {
    let doc = Document::load(path).expect("could not reload document");

    doc.get_pages()
        .into_values()
        .map(|page_id| {
            let mut current = Some(page_id);
            let mut depth = 0;

            while let Some(id) = current {
                depth += 1;
                if depth > 32 {
                    break;
                }

                let Ok(dict) = doc.get_dictionary(id) else {
                    break;
                };

                if let Ok(Object::Array(values)) = dict.get(b"MediaBox") {
                    // Producers write these as either integers or reals;
                    // printpdf uses reals, lopdf's own writer uses integers.
                    return values
                        .iter()
                        .map(|value| match value {
                            Object::Integer(number) => *number,
                            Object::Real(number) => number.round() as i64,
                            _ => 0,
                        })
                        .collect();
                }

                current = dict
                    .get(b"Parent")
                    .ok()
                    .and_then(|object| object.as_reference().ok());
            }

            Vec::new()
        })
        .collect()
}

/// Every distinct object id referenced from the page tree's `/Kids`.
pub fn page_ids(path: &Path) -> Vec<(u32, u16)> {
    let doc = Document::load(path).expect("could not reload document");
    doc.get_pages().into_values().collect()
}

/// Write a fixture and hand back its path, for tests that only need one page.
pub fn one_page(workspace: &Workspace, name: &str) -> PathBuf {
    workspace.document(name, 1, "Page")
}

/// Photo-like RGB pixels: a gradient with deterministic noise, so that they do
/// not compress down to nothing and resampling has something to lose.
pub fn photo_pixels(width: u32, height: u32) -> image::RgbImage {
    let mut pixels = image::RgbImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        // A flat gradient would survive any downsampling, and a photo-like
        // image is what these tests are standing in for.
        let r = (x * 7 + y * 13) as u8;
        let g = (x.wrapping_mul(y) % 251) as u8;
        let b = (x as i32 - y as i32).unsigned_abs() as u8;
        *pixel = image::Rgb([r, g, b]);
    }
    pixels
}

/// Build a JPEG of the given pixel size from [`photo_pixels`].
pub fn jpeg_bytes(width: u32, height: u32, quality: u8) -> Vec<u8> {
    use image::ImageEncoder;

    let pixels = photo_pixels(width, height);

    let mut buffer = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, quality)
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .expect("could not encode the fixture JPEG");

    buffer.into_inner()
}

/// How an image should appear on the page, in PDF points.
#[derive(Debug, Clone, Copy)]
pub struct ImagePlacement {
    pub width: f32,
    pub height: f32,
    pub draw: bool,
}

impl ImagePlacement {
    pub fn drawn(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            draw: true,
        }
    }

    /// Embedded but never invoked by the content stream.
    pub fn undrawn() -> Self {
        Self {
            width: 0.0,
            height: 0.0,
            draw: false,
        }
    }
}

/// A one-page A4 document containing a single JPEG image XObject.
pub fn build_with_image(
    pixel_width: u32,
    pixel_height: u32,
    placement: ImagePlacement,
) -> Document {
    let jpeg = jpeg_bytes(pixel_width, pixel_height, 92);
    let image = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => pixel_width as i64,
            "Height" => pixel_height as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg,
    );

    build_with_image_stream(image, placement)
}

/// How the samples of a Flate image are delta-encoded before compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Predictor {
    /// `/Predictor 2`: each sample minus the same component to its left.
    Tiff,
    /// `/Predictor 15`: PNG filters, using all five so that every filter type
    /// appears in the stream.
    Png,
}

/// Apply the PNG filter `filter` to one row, given the row above.
fn png_filter_row(filter: u8, row: &[u8], previous: &[u8], bpp: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(row.len() + 1);
    out.push(filter);

    for i in 0..row.len() {
        let left = if i >= bpp { row[i - bpp] } else { 0 };
        let up = previous[i];
        let up_left = if i >= bpp { previous[i - bpp] } else { 0 };

        let prediction = match filter {
            0 => 0,
            1 => left,
            2 => up,
            3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
            4 => {
                let (a, b, c) = (i16::from(left), i16::from(up), i16::from(up_left));
                let p = a + b - c;
                let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                if pa <= pb && pa <= pc {
                    left
                } else if pb <= pc {
                    up
                } else {
                    up_left
                }
            }
            _ => unreachable!("PNG defines filter types 0 to 4"),
        };
        out.push(row[i].wrapping_sub(prediction));
    }

    out
}

/// Delta-encode RGB samples with `predictor`, as a PDF writer would before
/// deflating them.
pub fn predict(pixels: &image::RgbImage, predictor: Predictor) -> Vec<u8> {
    const BPP: usize = 3;
    let row_bytes = pixels.width() as usize * BPP;
    let raw = pixels.as_raw();

    match predictor {
        Predictor::Tiff => {
            let mut out = raw.clone();
            for (row, original) in out.chunks_mut(row_bytes).zip(raw.chunks(row_bytes)) {
                for i in BPP..row.len() {
                    row[i] = original[i].wrapping_sub(original[i - BPP]);
                }
            }
            out
        }
        Predictor::Png => {
            let mut out = Vec::with_capacity(raw.len() + pixels.height() as usize);
            let mut previous = vec![0u8; row_bytes];
            for (index, row) in raw.chunks(row_bytes).enumerate() {
                // None, then Sub, then Up, Average and Paeth in turn. None and
                // Sub ignore the row above, so using each only once lets a
                // mistake in any later row carry down the rest of the image
                // rather than being reset a few rows on.
                let filter = match index {
                    0 => 0,
                    1 => 1,
                    _ => 2 + (index - 2) % 3,
                };
                out.extend(png_filter_row(filter as u8, row, &previous, BPP));
                previous.copy_from_slice(row);
            }
            out
        }
    }
}

/// Photo-like RGB pixels with smooth gradients under mild per-pixel grain.
///
/// Unlike [`photo_pixels`], the content that matters survives resampling and
/// JPEG nearly intact, so a test can compare a recompressed image against the
/// original closely. The grain keeps Flate from shrinking it to nothing, and
/// because the grain is not predictable, a predictor reversed wrongly leaves
/// errors that build up down the image instead of cancelling out.
pub fn soft_photo_pixels(width: u32, height: u32) -> image::RgbImage {
    let mut pixels = image::RgbImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 / width as f32, y as f32 / height as f32);

        // A cheap integer hash, giving grain in -8..=7.
        let hash = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(2_246_822_519))
            .wrapping_mul(3_266_489_917);
        let grain = (hash >> 28) as f32 - 8.0;

        let channel = |value: f32| (value + grain).clamp(0.0, 255.0) as u8;
        *pixel = image::Rgb([
            channel(40.0 + 170.0 * fx),
            channel(40.0 + 170.0 * fy),
            channel(128.0 + 80.0 * (6.0 * fx + 4.0 * fy).sin()),
        ]);
    }
    pixels
}

/// A one-page A4 document containing a single Flate-compressed RGB image of
/// [`soft_photo_pixels`], delta-encoded with `predictor`.
pub fn build_with_predicted_image(
    pixel_width: u32,
    pixel_height: u32,
    placement: ImagePlacement,
    predictor: Predictor,
) -> Document {
    use std::io::Write;

    let predicted = predict(&soft_photo_pixels(pixel_width, pixel_height), predictor);

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&predicted).unwrap();
    let deflated = encoder.finish().unwrap();

    let mut image = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => pixel_width as i64,
            "Height" => pixel_height as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
            "DecodeParms" => dictionary! {
                "Predictor" => match predictor {
                    Predictor::Tiff => 2,
                    Predictor::Png => 15,
                },
                "Colors" => 3,
                "BitsPerComponent" => 8,
                "Columns" => pixel_width as i64,
            },
        },
        deflated,
    );
    // Already deflated; compressing again on save would corrupt it.
    image.allows_compression = false;

    build_with_image_stream(image, placement)
}

/// A one-page A4 document drawing `image` as `/Im1` at (50, 100).
fn build_with_image_stream(image: Stream, placement: ImagePlacement) -> Document {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let image_id = doc.add_object(image);

    let resources_id = doc.add_object(dictionary! {
        "XObject" => dictionary! { "Im1" => image_id },
    });

    let operations = if placement.draw {
        vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    placement.width.into(),
                    0.into(),
                    0.into(),
                    placement.height.into(),
                    50.into(),
                    100.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Im1".to_vec())]),
            Operation::new("Q", vec![]),
        ]
    } else {
        vec![]
    };

    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().unwrap(),
    ));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), A4.0.into(), A4.1.into()],
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

    doc
}

/// The pixel dimensions and byte size of every image XObject in a document.
pub fn image_summaries(path: &Path) -> Vec<(u32, u32, usize)> {
    let doc = Document::load(path).expect("could not reload document");

    doc.objects
        .values()
        .filter_map(|object| {
            let Object::Stream(stream) = object else {
                return None;
            };
            if stream
                .dict
                .get(b"Subtype")
                .and_then(Object::as_name_str)
                .ok()
                != Some("Image")
            {
                return None;
            }

            let width = stream.dict.get(b"Width").and_then(Object::as_i64).ok()? as u32;
            let height = stream.dict.get(b"Height").and_then(Object::as_i64).ok()? as u32;

            Some((width, height, stream.content.len()))
        })
        .collect()
}

/// A one-page A4 document showing `words` at the given size, using a base-14
/// font so that it renders anywhere.
pub fn build_text_page(words: &str, size: f32) -> Document {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), size.into()]),
            Operation::new("Td", vec![72.into(), 600.into()]),
            Operation::new("Tj", vec![Object::string_literal(words)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), A4.0.into(), A4.1.into()],
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

    doc
}

/// A one-page document whose only content is the supplied JPEG.
pub fn build_with_jpeg(
    jpeg: Vec<u8>,
    pixel_width: u32,
    pixel_height: u32,
    placement: ImagePlacement,
) -> Document {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let image_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => pixel_width as i64,
            "Height" => pixel_height as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg,
    ));

    let resources_id = doc.add_object(dictionary! {
        "XObject" => dictionary! { "Im1" => image_id },
    });

    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    placement.width.into(),
                    0.into(),
                    0.into(),
                    placement.height.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Im1".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), A4.0.into(), A4.1.into()],
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

    doc
}
