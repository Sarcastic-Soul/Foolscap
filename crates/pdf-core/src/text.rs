//! PDF text string encoding.
//!
//! PDF text strings come in two flavours: UTF-16BE prefixed with a byte order
//! mark, and PDFDocEncoding, which agrees with Latin-1 over the range producers
//! actually use. `lopdf`'s own `as_string` runs `from_utf8_lossy` over the raw
//! bytes, which turns any UTF-16BE title into interleaved replacement
//! characters, so metadata reading goes through here instead.

const UTF16BE_BOM: [u8; 2] = [0xFE, 0xFF];

/// Decode a PDF text string into Rust text, guessing the encoding from the
/// leading byte order mark.
pub(crate) fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&UTF16BE_BOM) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }

    // PDFDocEncoding overlaps Latin-1 everywhere it matters, and a valid UTF-8
    // sequence is overwhelmingly more likely to be UTF-8 than the Latin-1
    // characters it would otherwise decode to.
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&byte| byte as char).collect(),
    }
}

/// Encode Rust text as a PDF text string.
///
/// ASCII is written literally, which keeps simple metadata readable in a hex
/// dump; anything else becomes UTF-16BE with a byte order mark, which is the
/// only encoding a PDF text string can carry non-Latin text in.
pub(crate) fn encode_pdf_string(text: &str) -> Vec<u8> {
    if text.is_ascii() {
        return text.as_bytes().to_vec();
    }

    let mut bytes = Vec::with_capacity(2 + text.len() * 2);
    bytes.extend_from_slice(&UTF16BE_BOM);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_round_trips_literally() {
        let encoded = encode_pdf_string("Quarterly Report");
        assert_eq!(encoded, b"Quarterly Report");
        assert_eq!(decode_pdf_string(&encoded), "Quarterly Report");
    }

    #[test]
    fn non_ascii_round_trips_through_utf16be() {
        for text in ["Résumé", "日本語の題名", "naïve — dash"] {
            let encoded = encode_pdf_string(text);
            assert_eq!(&encoded[..2], &UTF16BE_BOM);
            assert_eq!(decode_pdf_string(&encoded), text);
        }
    }

    #[test]
    fn utf16be_input_is_not_mangled() {
        // What a producer such as Word actually writes for "Té".
        let bytes = [0xFE, 0xFF, 0x00, b'T', 0x00, 0xE9];
        assert_eq!(decode_pdf_string(&bytes), "Té");
    }

    #[test]
    fn latin1_input_is_decoded_as_latin1() {
        // 0xE9 alone is not valid UTF-8, so it must be read as Latin-1.
        assert_eq!(decode_pdf_string(&[b'T', 0xE9]), "Té");
    }

    #[test]
    fn empty_and_odd_length_inputs_do_not_panic() {
        assert_eq!(decode_pdf_string(b""), "");
        assert_eq!(decode_pdf_string(&UTF16BE_BOM), "");
        // Truncated final code unit is dropped rather than panicking.
        assert_eq!(decode_pdf_string(&[0xFE, 0xFF, 0x00, b'T', 0x00]), "T");
    }
}
