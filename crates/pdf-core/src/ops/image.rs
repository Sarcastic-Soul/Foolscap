//! Shared decoding and encoding for image XObjects.
//!
//! Both [`compress`](super::compress) and [`filter`](super::filter) work on
//! decoded pixels: this module holds the one decode path and the one way of
//! building a replacement stream around new JPEG bytes, so the two passes
//! cannot drift apart.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageEncoder, ImageFormat};
use lopdf::{Dictionary, Object, Stream};

use super::compress::predictor::{unpredict, PredictorParams};
use super::compress::SkipReason;
use crate::error::Result;

/// A stencil mask, or an image with an explicit `/Mask`. Both cut a shape out
/// of the page, and rewriting the samples changes that shape.
///
/// `/SMask` is deliberately not in this list: a soft mask is a separate object
/// that survives the base image being replaced.
pub(crate) fn is_masked(dict: &Dictionary) -> bool {
    dict.get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false)
        || dict.get(b"Mask").is_ok()
}

/// Decode an image XObject to pixels.
///
/// Accepts DCT (JPEG) and Flate-compressed or uncompressed bitmaps in the
/// sRGB and grey colour spaces at 8 bits per component. Anything else is
/// reported as a [`SkipReason`] rather than an error: a pass over a whole
/// document must leave images it cannot read untouched, not fail.
pub(crate) fn decode_image(
    doc: &lopdf::Document,
    stream: &Stream,
) -> std::result::Result<DynamicImage, SkipReason> {
    let filters = stream.filters().unwrap_or_default();

    match filters.last().copied() {
        Some(b"DCTDecode") => {
            image::load_from_memory_with_format(&stream.content, ImageFormat::Jpeg)
                .map_err(|_| SkipReason::UnsupportedFilter)
        }
        Some(b"FlateDecode") | None => decode_raw(doc, stream),
        // JPXDecode, CCITTFaxDecode, JBIG2Decode and friends need decoders this
        // pass does not carry.
        Some(_) => Err(SkipReason::UnsupportedFilter),
    }
}

/// Decode an uncompressed or Flate-compressed bitmap.
fn decode_raw(
    doc: &lopdf::Document,
    stream: &Stream,
) -> std::result::Result<DynamicImage, SkipReason> {
    let bits = stream
        .dict
        .get(b"BitsPerComponent")
        .and_then(Object::as_i64)
        .unwrap_or(8);
    if bits != 8 {
        return Err(SkipReason::UnsupportedColorSpace);
    }

    let width = stream
        .dict
        .get(b"Width")
        .and_then(Object::as_i64)
        .map_err(|_| SkipReason::UnsupportedColorSpace)? as u32;
    let height = stream
        .dict
        .get(b"Height")
        .and_then(Object::as_i64)
        .map_err(|_| SkipReason::UnsupportedColorSpace)? as u32;

    let colour = stream
        .dict
        .get(b"ColorSpace")
        .and_then(Object::as_name)
        .unwrap_or(b"DeviceRGB");

    let samples = if stream.filters().unwrap_or_default().is_empty() {
        // Decode parameters belong to a filter; with no filter there are none.
        stream.content.clone()
    } else {
        let params = predictor_params(doc, &stream.dict)?;
        unpredict(inflate(&stream.content)?, &params).map_err(|error| {
            tracing::debug!(?error, ?params, "could not reverse the image predictor");
            SkipReason::UnsupportedFilter
        })?
    };

    match colour {
        b"DeviceRGB" | b"RGB" => image::RgbImage::from_raw(width, height, samples)
            .map(DynamicImage::ImageRgb8)
            .ok_or(SkipReason::UnsupportedColorSpace),
        b"DeviceGray" | b"G" => image::GrayImage::from_raw(width, height, samples)
            .map(DynamicImage::ImageLuma8)
            .ok_or(SkipReason::UnsupportedColorSpace),
        // Indexed, Separation, DeviceN, ICCBased and CMYK all need the colour
        // space resolved before the samples mean anything.
        _ => Err(SkipReason::UnsupportedColorSpace),
    }
}

/// The predictor parameters for the Flate filter, which is the last one in the
/// chain since that is the only shape [`decode_image`] accepts.
///
/// `/DecodeParms` is a dictionary for a single filter, or an array matching a
/// `/Filter` array; either may be an indirect reference. A missing entry, or
/// `null` in the array, means no parameters and therefore no predictor.
fn predictor_params(
    doc: &lopdf::Document,
    dict: &Dictionary,
) -> std::result::Result<PredictorParams, SkipReason> {
    let none = PredictorParams::from_dict(&Dictionary::new());

    let Ok(parms) = dict.get(b"DecodeParms") else {
        return Ok(none);
    };
    let unreadable = |_| SkipReason::UnsupportedFilter;

    let parms = match doc.dereference(parms).map_err(unreadable)?.1 {
        Object::Array(items) => match items.last() {
            Some(last) => doc.dereference(last).map_err(unreadable)?.1,
            None => return Ok(none),
        },
        other => other,
    };

    match parms {
        Object::Dictionary(parms) => Ok(PredictorParams::from_dict(parms)),
        Object::Null => Ok(none),
        _ => Err(SkipReason::UnsupportedFilter),
    }
}

pub(crate) fn inflate(bytes: &[u8]) -> std::result::Result<Vec<u8>, SkipReason> {
    use std::io::Read;

    let mut output = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .read_to_end(&mut output)
        .map_err(|_| SkipReason::UnsupportedFilter)?;

    Ok(output)
}

/// Encode pixels as JPEG at `quality`. JPEG carries no alpha, so anything with
/// a transparency channel is flattened to its colour channels; the PDF's own
/// `/SMask` still applies.
pub(crate) fn encode_jpeg(
    image: &DynamicImage,
    quality: u8,
) -> std::result::Result<Vec<u8>, SkipReason> {
    let rgb = image.to_rgb8();

    let mut buffer = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut buffer, quality)
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|_| SkipReason::NoSaving)?;

    Ok(buffer.into_inner())
}

/// Encode a single-channel grey buffer as JPEG.
pub(crate) fn encode_gray_jpeg(
    pixels: &image::GrayImage,
    quality: u8,
) -> std::result::Result<Vec<u8>, SkipReason> {
    let mut buffer = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut buffer, quality)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            image::ExtendedColorType::L8,
        )
        .map_err(|_| SkipReason::NoSaving)?;

    Ok(buffer.into_inner())
}

/// Rebuild the image stream around new JPEG bytes, keeping the entries that
/// still apply and dropping the ones that describe the old encoding.
pub(crate) fn build_image_stream(
    original: &Dictionary,
    image: &DynamicImage,
    encoded: Vec<u8>,
) -> Stream {
    let mut dict = original.clone();

    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", Object::Integer(image.width() as i64));
    dict.set("Height", Object::Integer(image.height() as i64));
    dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));

    // These describe the encoding that has just been replaced.
    dict.remove(b"DecodeParms");
    dict.remove(b"Decode");

    let mut stream = Stream::new(dict, encoded);
    // The bytes are already JPEG; wrapping them in Flate would only grow them.
    stream.allows_compression = false;
    stream
}

/// Rebuild the image stream around new grey JPEG bytes.
pub(crate) fn build_gray_image_stream(
    original: &Dictionary,
    pixels: &image::GrayImage,
    encoded: Vec<u8>,
) -> Stream {
    let mut dict = original.clone();

    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", Object::Integer(pixels.width() as i64));
    dict.set("Height", Object::Integer(pixels.height() as i64));
    dict.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));

    dict.remove(b"DecodeParms");
    dict.remove(b"Decode");

    let mut stream = Stream::new(dict, encoded);
    stream.allows_compression = false;
    stream
}

/// Serialise the document to memory and report its length.
pub(crate) fn serialized_len(doc: &crate::document::Document) -> Result<u64> {
    let mut buffer = Vec::new();
    doc.inner
        .clone()
        .save_to(&mut buffer)
        .map_err(|source| crate::PdfError::Internal(format!("could not serialise: {source}")))?;
    Ok(buffer.len() as u64)
}
