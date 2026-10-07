//! Undoing the row predictors that Flate (and LZW) streams may carry.
//!
//! A predictor stores each sample as a difference from its neighbours, which
//! makes smooth images deflate far better. The samples mean nothing until that
//! is reversed. PDF borrows two schemes (ISO 32000-1, 7.4.4.4):
//!
//! - `/Predictor 2`, the TIFF horizontal predictor: each sample is stored as
//!   the difference from the same component of the pixel to its left.
//! - `/Predictor 10` to `15`, the PNG filters: every row starts with a byte
//!   naming the filter used for that row, whatever the `/Predictor` value
//!   says, so all six values are decoded the same way.

use lopdf::{Dictionary, Object};

/// Why a predicted stream could not be reversed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PredictorError {
    /// A predictor number the PDF specification does not define.
    UnknownPredictor(i64),
    /// A row began with a PNG filter type outside 0 to 4.
    UnknownFilterType(u8),
    /// A component size this pass does not handle for this predictor.
    UnsupportedBitDepth(i64),
    /// `/Colors`, `/BitsPerComponent` or `/Columns` is zero, negative or absurd.
    BadParameters,
}

/// The entries of a `/DecodeParms` dictionary that describe a predictor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PredictorParams {
    pub predictor: i64,
    pub colors: i64,
    pub bits_per_component: i64,
    pub columns: i64,
}

impl PredictorParams {
    /// Read the parameters, using the defaults the specification gives for
    /// any entry that is missing.
    pub fn from_dict(parms: &Dictionary) -> Self {
        let int =
            |key: &[u8], default: i64| parms.get(key).and_then(Object::as_i64).unwrap_or(default);

        Self {
            predictor: int(b"Predictor", 1),
            colors: int(b"Colors", 1),
            bits_per_component: int(b"BitsPerComponent", 8),
            columns: int(b"Columns", 1),
        }
    }

    /// Bytes in one row of samples, not counting a PNG filter-type byte.
    fn row_bytes(&self) -> Result<usize, PredictorError> {
        let bits = self
            .colors
            .checked_mul(self.bits_per_component)
            .and_then(|bits| bits.checked_mul(self.columns))
            .ok_or(PredictorError::BadParameters)?;
        usize::try_from(bits)
            .map(|bits| bits.div_ceil(8))
            .map_err(|_| PredictorError::BadParameters)
    }

    /// Bytes per whole pixel, rounded up to one: the distance PNG filters
    /// look back to find "the pixel to the left".
    fn bytes_per_pixel(&self) -> Result<usize, PredictorError> {
        let bits = self
            .colors
            .checked_mul(self.bits_per_component)
            .ok_or(PredictorError::BadParameters)?;
        usize::try_from(bits)
            .map(|bits| bits.div_ceil(8).max(1))
            .map_err(|_| PredictorError::BadParameters)
    }

    fn validate(&self) -> Result<(), PredictorError> {
        if self.colors < 1 || self.columns < 1 {
            return Err(PredictorError::BadParameters);
        }
        if !matches!(self.bits_per_component, 1 | 2 | 4 | 8 | 16) {
            return Err(PredictorError::BadParameters);
        }
        Ok(())
    }
}

/// Reverse whatever predictor `params` names, returning the raw samples.
pub(super) fn unpredict(
    data: Vec<u8>,
    params: &PredictorParams,
) -> Result<Vec<u8>, PredictorError> {
    match params.predictor {
        1 => Ok(data),
        2 => {
            params.validate()?;
            tiff_unpredict(data, params)
        }
        10..=15 => {
            params.validate()?;
            png_unfilter(&data, params.bytes_per_pixel()?, params.row_bytes()?)
        }
        other => Err(PredictorError::UnknownPredictor(other)),
    }
}

/// Reverse the TIFF horizontal predictor.
///
/// Only 8-bit components are handled. At other depths the differences are
/// taken between packed bit fields or big-endian pairs, which nothing in this
/// pass could use anyway, since the image decoder also wants 8 bits.
fn tiff_unpredict(mut data: Vec<u8>, params: &PredictorParams) -> Result<Vec<u8>, PredictorError> {
    if params.bits_per_component != 8 {
        return Err(PredictorError::UnsupportedBitDepth(
            params.bits_per_component,
        ));
    }

    let row_bytes = params.row_bytes()?;
    let colors = params.colors as usize;

    for row in data.chunks_mut(row_bytes) {
        for i in colors..row.len() {
            row[i] = row[i].wrapping_add(row[i - colors]);
        }
    }

    Ok(data)
}

/// Reverse PNG row filters.
///
/// `bpp` is the byte distance to the corresponding byte of the previous pixel,
/// and `row_bytes` the length of a row without its leading filter-type byte.
/// The filters work on bytes rather than samples, so this is correct for any
/// component depth; the caller decides whether it can use the result.
///
/// A trailing partial row, which some writers leave behind, is dropped.
pub(super) fn png_unfilter(
    data: &[u8],
    bpp: usize,
    row_bytes: usize,
) -> Result<Vec<u8>, PredictorError> {
    if bpp == 0 || row_bytes == 0 {
        return Err(PredictorError::BadParameters);
    }

    let rows = data.len() / (row_bytes + 1);
    let mut output = vec![0u8; rows * row_bytes];
    let mut previous = vec![0u8; row_bytes];

    for (encoded, decoded) in data
        .chunks_exact(row_bytes + 1)
        .zip(output.chunks_exact_mut(row_bytes))
    {
        let (filter, samples) = (encoded[0], &encoded[1..]);

        for i in 0..row_bytes {
            let left = if i >= bpp { decoded[i - bpp] } else { 0 };
            let up = previous[i];
            let up_left = if i >= bpp { previous[i - bpp] } else { 0 };

            let prediction = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth(left, up, up_left),
                other => return Err(PredictorError::UnknownFilterType(other)),
            };
            decoded[i] = samples[i].wrapping_add(prediction);
        }

        previous.copy_from_slice(decoded);
    }

    Ok(output)
}

/// The Paeth predictor: whichever of left, up and up-left is closest to
/// `left + up - up_left`, preferring them in that order on a tie.
fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rows of two RGB pixels each. In the second pixel of the second row,
    /// Paeth picks left for the first byte, up for the second and up-left for
    /// the third, so each filter predicts something different.
    const ROW_0: [u8; 6] = [10, 20, 30, 40, 50, 60];
    const ROW_1: [u8; 6] = [50, 25, 10, 100, 90, 80];

    /// Difference modulo 256, as a filter stores it.
    fn d(value: u8, prediction: u8) -> u8 {
        value.wrapping_sub(prediction)
    }

    fn decode(filter: u8, encoded_row_1: [u8; 6]) -> Vec<u8> {
        // The first row is stored unfiltered so the second has a known "up".
        let mut data = vec![0];
        data.extend_from_slice(&ROW_0);
        data.push(filter);
        data.extend_from_slice(&encoded_row_1);

        png_unfilter(&data, 3, 6).unwrap()
    }

    fn expected() -> Vec<u8> {
        [ROW_0, ROW_1].concat()
    }

    /// The second row encoded with the Up filter.
    fn up_encoded() -> [u8; 6] {
        [
            d(50, 10),
            d(25, 20),
            d(10, 30),
            d(100, 40),
            d(90, 50),
            d(80, 60),
        ]
    }

    #[test]
    fn png_none_copies_the_row() {
        assert_eq!(decode(0, ROW_1), expected());
    }

    #[test]
    fn png_sub_adds_the_pixel_to_the_left() {
        // The first pixel has no left neighbour; the second is relative to it.
        let encoded = [50, 25, 10, d(100, 50), d(90, 25), d(80, 10)];
        assert_eq!(decode(1, encoded), expected());
    }

    #[test]
    fn png_up_adds_the_pixel_above() {
        // 10 - 30 wraps, which the decoder must undo by wrapping back.
        assert_eq!(decode(2, up_encoded()), expected());
    }

    #[test]
    fn png_average_adds_the_mean_of_left_and_up() {
        // First pixel: left is 0, so the mean is half of up, rounded down.
        // Second pixel: the means of (50, 40), (25, 50) and (10, 60) are 45,
        // 37 and 35.
        let encoded = [
            d(50, 5),
            d(25, 10),
            d(10, 15),
            d(100, 45),
            d(90, 37),
            d(80, 35),
        ];
        assert_eq!(decode(3, encoded), expected());
    }

    #[test]
    fn png_paeth_adds_the_nearest_neighbour() {
        // First pixel: left and up-left are 0, so Paeth picks up.
        // Second pixel:
        //   left 50, up 40, up-left 10: p = 80, nearest is left (50).
        //   left 25, up 50, up-left 20: p = 55, nearest is up (50).
        //   left 10, up 60, up-left 30: p = 40, nearest is up-left (30).
        let encoded = [
            d(50, 10),
            d(25, 20),
            d(10, 30),
            d(100, 50),
            d(90, 50),
            d(80, 30),
        ];
        assert_eq!(decode(4, encoded), expected());
    }

    #[test]
    fn paeth_breaks_ties_in_the_specified_order() {
        // All equal: left wins.
        assert_eq!(paeth(7, 7, 7), 7);
        // p = 30: pa = 20, pb = 10, pc = 30, so up.
        assert_eq!(paeth(10, 20, 0), 20);
        // p = 0: pa = 10, pb = 10, pc = 20; the tie goes to left.
        assert_eq!(paeth(10, 10, 20), 10);
        // p = 25: pa = 5, pb = 5, pc = 0, so up-left.
        assert_eq!(paeth(20, 30, 25), 25);
        // p = 15: pa = 5, pb = 15, pc = 20, so left.
        assert_eq!(paeth(20, 30, 35), 20);
        // p = 20: pa = 10, pb = 5, pc = 5; the tie goes to up over up-left.
        assert_eq!(paeth(10, 25, 15), 25);
    }

    #[test]
    fn png_filters_wrap_modulo_256() {
        // 200 + 100 overflows a byte; the specification says to wrap.
        let data = [1, 200, 100];
        assert_eq!(png_unfilter(&data, 1, 2).unwrap(), [200, 44]);
    }

    #[test]
    fn mixed_filters_across_rows_decode_together() {
        // Sub on the first row, Up on the second.
        let mut data = vec![1, 10, 20, 30, d(40, 10), d(50, 20), d(60, 30)];
        data.push(2);
        data.extend_from_slice(&up_encoded());

        assert_eq!(png_unfilter(&data, 3, 6).unwrap(), expected());
    }

    #[test]
    fn an_unknown_png_filter_type_is_an_error() {
        let data = [5, 1, 2, 3];
        assert_eq!(
            png_unfilter(&data, 1, 3),
            Err(PredictorError::UnknownFilterType(5))
        );
    }

    #[test]
    fn a_trailing_partial_row_is_dropped() {
        let data = [0, 1, 2, 3, 0, 9];
        assert_eq!(png_unfilter(&data, 1, 3).unwrap(), [1, 2, 3]);
    }

    #[test]
    fn every_png_predictor_value_reads_the_per_row_filter_byte() {
        let mut data = vec![0];
        data.extend_from_slice(&ROW_0);
        data.push(2);
        data.extend_from_slice(&up_encoded());

        for predictor in 10..=15 {
            let params = PredictorParams {
                predictor,
                colors: 3,
                bits_per_component: 8,
                columns: 2,
            };
            assert_eq!(unpredict(data.clone(), &params).unwrap(), expected());
        }
    }

    #[test]
    fn tiff_predictor_adds_the_same_component_of_the_previous_pixel() {
        let params = PredictorParams {
            predictor: 2,
            colors: 3,
            bits_per_component: 8,
            columns: 2,
        };
        let mut encoded = vec![10, 20, 30, 30, 30, 30];
        encoded.extend_from_slice(&[50, 25, 10, d(100, 50), d(90, 25), d(80, 10)]);

        assert_eq!(unpredict(encoded, &params).unwrap(), expected());
    }

    #[test]
    fn tiff_predictor_at_sixteen_bits_is_refused() {
        let params = PredictorParams {
            predictor: 2,
            colors: 1,
            bits_per_component: 16,
            columns: 4,
        };
        assert_eq!(
            unpredict(vec![0; 8], &params),
            Err(PredictorError::UnsupportedBitDepth(16))
        );
    }

    #[test]
    fn no_predictor_passes_the_data_through() {
        let params = PredictorParams::from_dict(&Dictionary::new());
        assert_eq!(params.predictor, 1);
        assert_eq!(unpredict(vec![1, 2, 3], &params).unwrap(), [1, 2, 3]);
    }

    #[test]
    fn missing_entries_take_the_specified_defaults() {
        let mut dict = Dictionary::new();
        dict.set("Predictor", Object::Integer(12));
        let params = PredictorParams::from_dict(&dict);

        assert_eq!(params.colors, 1);
        assert_eq!(params.bits_per_component, 8);
        assert_eq!(params.columns, 1);
    }

    #[test]
    fn nonsense_parameters_are_refused() {
        let params = PredictorParams {
            predictor: 12,
            colors: 0,
            bits_per_component: 8,
            columns: 10,
        };
        assert_eq!(
            unpredict(vec![0; 11], &params),
            Err(PredictorError::BadParameters)
        );

        let params = PredictorParams {
            predictor: 7,
            ..params
        };
        assert_eq!(
            unpredict(vec![], &params),
            Err(PredictorError::UnknownPredictor(7))
        );
    }
}
