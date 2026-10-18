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

    let samples = unfiltered_samples(doc, stream)?;

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

/// The raw sample bytes of an uncompressed or Flate-compressed image, with any
/// predictor reversed.
fn unfiltered_samples(
    doc: &lopdf::Document,
    stream: &Stream,
) -> std::result::Result<Vec<u8>, SkipReason> {
    if stream.filters().unwrap_or_default().is_empty() {
        // Decode parameters belong to a filter; with no filter there are none.
        return Ok(stream.content.clone());
    }

    let params = predictor_params(doc, &stream.dict)?;
    unpredict(inflate(&stream.content)?, &params).map_err(|error| {
        tracing::debug!(?error, ?params, "could not reverse the image predictor");
        SkipReason::UnsupportedFilter
    })
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

// ------------------------------------------------------------- pixel editing
//
// Redaction has to change the pixels themselves, so it needs to read more
// kinds of image than recompression does (indexed palettes, ICC profiles,
// 1, 2 and 4-bit samples, stencil masks) and to write them back without losing
// anything outside the edited area.

/// Pixels decoded so that part of them can be overwritten.
enum Raster {
    Rgb(image::RgbImage),
    Gray(image::GrayImage),
    /// A 1-bit stencil mask: packed rows of `(width + 7) / 8` bytes.
    Stencil {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
}

/// A colour space reduced to what is needed to turn samples into 8-bit pixels.
enum Space {
    Gray,
    Rgb,
    Indexed {
        /// Components in each palette entry: 1 or 3.
        channels: usize,
        palette: Vec<u8>,
    },
}

fn resolve_space(
    doc: &lopdf::Document,
    object: &Object,
    depth: u8,
) -> std::result::Result<Space, SkipReason> {
    const UNSUPPORTED: SkipReason = SkipReason::UnsupportedColorSpace;
    if depth > 4 {
        return Err(UNSUPPORTED);
    }
    let object = doc.dereference(object).map_err(|_| UNSUPPORTED)?.1;

    let by_name = |name: &[u8]| match name {
        b"DeviceGray" | b"G" | b"CalGray" => Ok(Space::Gray),
        b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(Space::Rgb),
        _ => Err(UNSUPPORTED),
    };

    match object {
        Object::Name(name) => by_name(name),
        Object::Array(items) => {
            let family = items
                .first()
                .and_then(|item| item.as_name().ok())
                .ok_or(UNSUPPORTED)?;
            match family {
                b"ICCBased" => {
                    let profile = items
                        .get(1)
                        .and_then(|item| doc.dereference(item).ok())
                        .and_then(|(_, item)| item.as_stream().ok())
                        .ok_or(UNSUPPORTED)?;
                    match profile.dict.get(b"N").and_then(Object::as_i64) {
                        Ok(1) => Ok(Space::Gray),
                        Ok(3) => Ok(Space::Rgb),
                        _ => Err(UNSUPPORTED),
                    }
                }
                b"Indexed" | b"I" => {
                    let base = resolve_space(doc, items.get(1).ok_or(UNSUPPORTED)?, depth + 1)?;
                    let channels = match base {
                        Space::Gray => 1,
                        Space::Rgb => 3,
                        Space::Indexed { .. } => return Err(UNSUPPORTED),
                    };
                    let lookup = items.get(3).ok_or(UNSUPPORTED)?;
                    let palette = match doc.dereference(lookup).map_err(|_| UNSUPPORTED)?.1 {
                        Object::String(bytes, _) => bytes.clone(),
                        Object::Stream(stream) => {
                            stream.get_plain_content().map_err(|_| UNSUPPORTED)?
                        }
                        _ => return Err(UNSUPPORTED),
                    };
                    Ok(Space::Indexed { channels, palette })
                }
                other => by_name(other),
            }
        }
        _ => Err(UNSUPPORTED),
    }
}

/// True when `/Decode` is absent or says what the default says.
fn decode_is_default(doc: &lopdf::Document, dict: &Dictionary, max: f32) -> bool {
    let Ok(decode) = dict.get(b"Decode") else {
        return true;
    };
    let Ok((_, Object::Array(values))) = doc.dereference(decode) else {
        return false;
    };
    values.iter().enumerate().all(|(index, value)| {
        let expected = if index % 2 == 0 { 0.0 } else { max };
        value.as_float().is_ok_and(|v| (v - expected).abs() < 1e-3)
    })
}

/// Turn packed samples of `bits` each into one byte per sample, unscaled.
fn unpack_samples(
    data: &[u8],
    width: usize,
    height: usize,
    components: usize,
    bits: usize,
) -> Option<Vec<u8>> {
    let per_row = width.checked_mul(components)?;
    let stride = per_row.checked_mul(bits)?.div_ceil(8);
    if data.len() < stride.checked_mul(height)? {
        return None;
    }

    let mut out = Vec::with_capacity(per_row.checked_mul(height)?);
    for row in data.chunks(stride).take(height) {
        for index in 0..per_row {
            out.push(match bits {
                8 => row[index],
                _ => {
                    let bit = index * bits;
                    let shift = 8 - bits - (bit % 8);
                    (row[bit / 8] >> shift) & ((1u8 << bits) - 1)
                }
            });
        }
    }
    Some(out)
}

/// Decode `stream` to pixels that can be edited. The flag says whether the
/// source was a JPEG, so it can go back as one.
fn decode_for_editing(
    doc: &lopdf::Document,
    stream: &Stream,
) -> std::result::Result<(Raster, bool), SkipReason> {
    let dict = &stream.dict;
    let dimension = |key: &[u8]| {
        dict.get(key)
            .and_then(Object::as_i64)
            .ok()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or(SkipReason::UnsupportedColorSpace)
    };
    let (width, height) = (dimension(b"Width")?, dimension(b"Height")?);
    let filters = stream.filters().unwrap_or_default();

    let first_filter = filters.first().copied();
    if filters.len() > 1 || !matches!(first_filter, None | Some(b"FlateDecode" | b"DCTDecode")) {
        return Err(SkipReason::UnsupportedFilter);
    }

    let stencil = dict
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false);
    if stencil {
        if first_filter == Some(b"DCTDecode".as_slice()) {
            return Err(SkipReason::UnsupportedFilter);
        }
        let data = unfiltered_samples(doc, stream)?;
        let stride = (width as usize).div_ceil(8);
        if data.len() < stride * height as usize {
            return Err(SkipReason::UnsupportedFilter);
        }
        return Ok((
            Raster::Stencil {
                width,
                height,
                data,
            },
            false,
        ));
    }

    if dict.get(b"Mask").is_ok_and(|mask| mask.as_array().is_ok()) {
        // A colour-key mask would turn the blanked pixels transparent.
        return Err(SkipReason::Mask);
    }

    if first_filter == Some(b"DCTDecode".as_slice()) {
        if !decode_is_default(doc, dict, 1.0) {
            return Err(SkipReason::UnsupportedColorSpace);
        }
        let decoded = image::load_from_memory_with_format(&stream.content, ImageFormat::Jpeg)
            .map_err(|_| SkipReason::UnsupportedFilter)?;
        let raster = match decoded.color().channel_count() {
            1 | 2 => Raster::Gray(decoded.to_luma8()),
            _ => Raster::Rgb(decoded.to_rgb8()),
        };
        return Ok((raster, true));
    }

    let bits = dict
        .get(b"BitsPerComponent")
        .and_then(Object::as_i64)
        .unwrap_or(8) as usize;
    if !matches!(bits, 1 | 2 | 4 | 8) {
        return Err(SkipReason::UnsupportedColorSpace);
    }
    let space = resolve_space(
        doc,
        dict.get(b"ColorSpace")
            .map_err(|_| SkipReason::UnsupportedColorSpace)?,
        0,
    )?;
    let samples = unfiltered_samples(doc, stream)?;
    let (w, h) = (width as usize, height as usize);
    let top = ((1u32 << bits) - 1) as f32;
    let scale = |value: u8| (f32::from(value) * 255.0 / top).round() as u8;
    let bad = SkipReason::UnsupportedColorSpace;

    let raster = match space {
        Space::Gray => {
            if !decode_is_default(doc, dict, 1.0) {
                return Err(bad);
            }
            let values = unpack_samples(&samples, w, h, 1, bits).ok_or(bad)?;
            let pixels: Vec<u8> = values.into_iter().map(scale).collect();
            Raster::Gray(image::GrayImage::from_raw(width, height, pixels).ok_or(bad)?)
        }
        Space::Rgb => {
            if !decode_is_default(doc, dict, 1.0) {
                return Err(bad);
            }
            let values = unpack_samples(&samples, w, h, 3, bits).ok_or(bad)?;
            let pixels: Vec<u8> = values.into_iter().map(scale).collect();
            Raster::Rgb(image::RgbImage::from_raw(width, height, pixels).ok_or(bad)?)
        }
        Space::Indexed { channels, palette } => {
            if !decode_is_default(doc, dict, top) {
                return Err(bad);
            }
            let indexes = unpack_samples(&samples, w, h, 1, bits).ok_or(bad)?;
            let entry = |index: u8, channel: usize| {
                palette
                    .get(usize::from(index) * channels + channel)
                    .copied()
                    .unwrap_or(0)
            };
            if channels == 1 {
                let pixels: Vec<u8> = indexes.iter().map(|&i| entry(i, 0)).collect();
                Raster::Gray(image::GrayImage::from_raw(width, height, pixels).ok_or(bad)?)
            } else {
                let pixels: Vec<u8> = indexes
                    .iter()
                    .flat_map(|&i| [entry(i, 0), entry(i, 1), entry(i, 2)])
                    .collect();
                Raster::Rgb(image::RgbImage::from_raw(width, height, pixels).ok_or(bad)?)
            }
        }
    };
    Ok((raster, false))
}

/// A Flate image stream holding `samples`, with the dictionary entries that
/// described the old encoding replaced.
fn build_flate_stream(
    original: &Dictionary,
    width: u32,
    height: u32,
    colour: &'static str,
    samples: &[u8],
) -> std::result::Result<Stream, SkipReason> {
    let mut dict = original.clone();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", Object::Integer(i64::from(width)));
    dict.set("Height", Object::Integer(i64::from(height)));
    dict.set("ColorSpace", Object::Name(colour.as_bytes().to_vec()));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.remove(b"DecodeParms");
    dict.remove(b"Decode");

    Ok(flate_wrap(dict, samples))
}

fn flate_wrap(mut dict: Dictionary, samples: &[u8]) -> Stream {
    use std::io::Write;

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing to a Vec cannot fail.
    let _ = encoder.write_all(samples);
    let compressed = encoder.finish().unwrap_or_default();

    dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    dict.set("Length", Object::Integer(compressed.len() as i64));
    let mut stream = Stream::new(dict, compressed);
    stream.allows_compression = false;
    stream
}

/// Decode `stream`, paint the pixels that `covered` selects black, and return
/// a replacement stream; the original is untouched.
///
/// `window` is `(x0, y0, x1, y1)` in pixels, end-exclusive: only pixels inside
/// it are offered to `covered`. Black is the fill of the redaction box. A
/// stencil mask has no colour of its own, so its covered samples are set to
/// "leave unpainted" instead.
///
/// Fails with a [`SkipReason`] for anything this module cannot decode. The
/// caller must then not leave the image as it is.
pub(crate) fn blank_pixels(
    doc: &lopdf::Document,
    stream: &Stream,
    window: (u32, u32, u32, u32),
    covered: &dyn Fn(u32, u32) -> bool,
) -> std::result::Result<Stream, SkipReason> {
    let (mut raster, was_jpeg) = decode_for_editing(doc, stream)?;
    let (x0, y0, x1, y1) = window;

    match &mut raster {
        Raster::Rgb(pixels) => {
            let (x1, y1) = (x1.min(pixels.width()), y1.min(pixels.height()));
            for y in y0..y1 {
                for x in x0..x1 {
                    if covered(x, y) {
                        pixels.put_pixel(x, y, image::Rgb([0, 0, 0]));
                    }
                }
            }
        }
        Raster::Gray(pixels) => {
            let (x1, y1) = (x1.min(pixels.width()), y1.min(pixels.height()));
            for y in y0..y1 {
                for x in x0..x1 {
                    if covered(x, y) {
                        pixels.put_pixel(x, y, image::Luma([0]));
                    }
                }
            }
        }
        Raster::Stencil {
            width,
            height,
            data,
        } => {
            let stride = (*width as usize).div_ceil(8);
            // With the default /Decode [0 1] a 0 bit paints and a 1 leaves the
            // page showing; /Decode [1 0] swaps them.
            let inverted = stream
                .dict
                .get(b"Decode")
                .ok()
                .and_then(|decode| doc.dereference(decode).ok())
                .and_then(|(_, decode)| decode.as_array().ok())
                .and_then(|values| values.first())
                .and_then(|first| first.as_float().ok())
                .is_some_and(|first| first > 0.5);
            let (x1, y1) = (x1.min(*width), y1.min(*height));
            for y in y0..y1 {
                for x in x0..x1 {
                    if covered(x, y) {
                        let byte = &mut data[y as usize * stride + x as usize / 8];
                        let bit = 0x80u8 >> (x % 8);
                        if inverted {
                            *byte &= !bit;
                        } else {
                            *byte |= bit;
                        }
                    }
                }
            }
        }
    }

    match raster {
        Raster::Rgb(pixels) => {
            if was_jpeg {
                let image = DynamicImage::ImageRgb8(pixels);
                let encoded = encode_jpeg(&image, 92)?;
                Ok(build_image_stream(&stream.dict, &image, encoded))
            } else {
                build_flate_stream(
                    &stream.dict,
                    pixels.width(),
                    pixels.height(),
                    "DeviceRGB",
                    pixels.as_raw(),
                )
            }
        }
        Raster::Gray(pixels) => {
            if was_jpeg {
                let encoded = encode_gray_jpeg(&pixels, 92)?;
                Ok(build_gray_image_stream(&stream.dict, &pixels, encoded))
            } else {
                build_flate_stream(
                    &stream.dict,
                    pixels.width(),
                    pixels.height(),
                    "DeviceGray",
                    pixels.as_raw(),
                )
            }
        }
        Raster::Stencil { data, .. } => {
            let mut dict = stream.dict.clone();
            dict.remove(b"DecodeParms");
            Ok(flate_wrap(dict, &data))
        }
    }
}
