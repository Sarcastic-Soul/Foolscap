//! Pull every image embedded in a document out to its own file.
//!
//! Walks the object table for image XObjects and writes each one in the
//! format it is already stored in where possible: JPEG streams go out as
//! `.jpg` bytes untouched, everything else is decoded and written as PNG.
//! Images that cannot be decoded are skipped rather than failing the run.

use std::path::{Path, PathBuf};

use lopdf::{Object, ObjectId};

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::ops::image::decode_image;

/// Extract every embedded image in `input` into `out_dir`.
pub fn extract_images(
    input: &Path,
    out_dir: &Path,
    password: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let doc = Document::open_with(input, password)?;
    std::fs::create_dir_all(out_dir).map_err(|source| PdfError::Io {
        path: out_dir.to_path_buf(),
        source,
    })?;

    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_string());

    let mut image_ids: Vec<ObjectId> = doc
        .inner
        .objects
        .iter()
        .filter_map(|(id, object)| {
            let Object::Stream(stream) = object else {
                return None;
            };
            let subtype = stream.dict.get(b"Subtype").ok()?.as_name().ok()?;
            (subtype == b"Image").then_some(*id)
        })
        .collect();
    image_ids.sort();

    let mut written = Vec::new();
    let mut index = 0usize;
    for id in image_ids {
        let Object::Stream(stream) = doc.inner.objects.get(&id).cloned().unwrap_or(Object::Null)
        else {
            continue;
        };

        let filters = stream.filters().unwrap_or_default();
        let is_jpeg = filters.last().copied() == Some(b"DCTDecode");

        if is_jpeg {
            index += 1;
            let path = out_dir.join(format!("{stem}-image-{index}.jpg"));
            std::fs::write(&path, &stream.content).map_err(|source| PdfError::Io {
                path: path.clone(),
                source,
            })?;
            written.push(path);
        } else {
            match decode_image(&doc.inner, &stream) {
                Ok(image) => {
                    index += 1;
                    let path = out_dir.join(format!("{stem}-image-{index}.png"));
                    image.save(&path).map_err(|source| PdfError::Io {
                        path: path.clone(),
                        source: std::io::Error::other(source.to_string()),
                    })?;
                    written.push(path);
                }
                Err(_) => {
                    tracing::debug!(?id, "could not decode image, skipping");
                }
            }
        }
    }

    Ok(written)
}
