//! PDF repair: load corrupt files with relaxed parsing and rebuild structure.

use std::path::Path;

use lopdf::{Document as LopdfDocument, Object};

use crate::document::Document;
use crate::error::{PdfError, Result};

/// Summary of what the repair operation fixed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepairReport {
    /// Number of objects found by scanning the file, when its cross-reference
    /// table was too damaged to read.
    pub recovered_objects: usize,
    /// Number of unreachable objects dropped while rebuilding the file.
    pub dropped_objects: usize,
    /// Number of stream lengths corrected.
    pub fixed_streams: usize,
    /// Non-fatal issues encountered during repair.
    pub warnings: Vec<String>,
}

impl RepairReport {
    /// True when nothing was changed.
    pub fn is_clean(&self) -> bool {
        self.recovered_objects == 0
            && self.dropped_objects == 0
            && self.fixed_streams == 0
            && self.warnings.is_empty()
    }
}

/// Attempt to load and repair a PDF from disk, writing the result to `out_path`.
///
/// This uses relaxed parsing to tolerate:
/// - Truncated stream data (length mismatch)
/// - Damaged or missing cross-reference tables
/// - Corrupt trailer dictionaries
/// - Orphan objects not reachable from the trailer
///
/// Returns a [`RepairReport`] describing what was fixed.
pub fn repair(input: &Path, out_path: &Path) -> Result<RepairReport> {
    if !input.exists() {
        return Err(PdfError::NotFound(input.to_path_buf()));
    }
    let bytes = std::fs::read(input).map_err(|source| PdfError::Io {
        path: input.to_path_buf(),
        source,
    })?;

    let mut report = RepairReport::default();

    let mut bytes = bytes;

    // `lopdf` trusts a stream's declared /Length and quietly adopts whatever
    // it reads, so a wrong length has to be caught in the raw bytes. Fixing
    // one moves every later offset, so the table is rebuilt afterwards.
    let patched = patch_stream_lengths(&bytes);
    let force_rebuild = patched.is_some();
    if let Some((fixed_bytes, count)) = patched {
        report.fixed_streams += count;
        bytes = fixed_bytes;
    }

    // Try the file's own cross-reference table first; scan for objects when
    // that fails or the lengths were patched.
    let mut doc = if force_rebuild {
        recover_document(&bytes, input, &mut report)?
    } else {
        match LopdfDocument::load_mem(&bytes) {
            Ok(doc) => rescue_lost_pages(doc, &bytes, input, &mut report),
            Err(error) => {
                tracing::debug!(%error, "initial load failed, scanning for objects");
                recover_document(&bytes, input, &mut report)?
            }
        }
    };

    // A file that needs a password loads as an empty shell; repairing that
    // would write the shell out.
    if doc.trailer.get(b"Encrypt").is_ok() {
        return Err(PdfError::Encrypted(input.to_path_buf()));
    }
    // Saved files are whole and plain, so forget how the input was protected.
    doc.encryption_state.take();

    repair_inner(&mut doc, &mut report);

    // Save the repaired document.
    Document::from_lopdf(doc, None).save(out_path)?;

    Ok(report)
}

/// The repair steps shared by [`repair`] and [`repair_loaded`].
fn repair_inner(doc: &mut LopdfDocument, report: &mut RepairReport) {
    // Fix stream lengths that don't match actual data.
    report.fixed_streams += fix_stream_lengths(doc);

    // Cross-reference and object streams are bookkeeping that the writer
    // regenerates; they are not content, so dropping them is not a repair.
    let bookkeeping: Vec<lopdf::ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, object)| {
            object.as_stream().is_ok_and(|stream| {
                stream
                    .dict
                    .get(b"Type")
                    .and_then(Object::as_name)
                    .is_ok_and(|name| name == b"XRef" || name == b"ObjStm")
            })
        })
        .map(|(id, _)| *id)
        .collect();
    for id in bookkeeping {
        doc.objects.remove(&id);
    }

    // Rebuild the cross-reference table by renumbering and pruning.
    let before = doc.objects.len();
    doc.renumber_objects();
    doc.prune_objects();
    report.dropped_objects = before.saturating_sub(doc.objects.len());

    // Make sure the trailer has the required entries.
    fix_trailer(doc, report);
}

/// Last-resort recovery: scan the raw bytes for objects and write a fresh
/// cross-reference table and trailer after them.
///
/// Used when the file's own table or trailer cannot be read. The bytes are
/// kept as they are and a new table is appended, so `lopdf` finds the new
/// `startxref` at the end of the file and loads every object the scan found.
/// Objects held inside compressed object streams cannot be found this way.
fn recover_document(
    bytes: &[u8],
    input: &Path,
    report: &mut RepairReport,
) -> Result<LopdfDocument> {
    let malformed = |reason: &str| PdfError::Malformed {
        path: input.to_path_buf(),
        reason: reason.to_string(),
    };

    let found = scan_objects(bytes);
    if found.is_empty() {
        return Err(malformed("no PDF objects could be found"));
    }

    // An encrypted file cannot be rebuilt without its password, and the
    // encryption entry would be lost with the old trailer.
    if find_last(bytes, b"/Encrypt").is_some() {
        return Err(PdfError::Encrypted(input.to_path_buf()));
    }

    let root = trailer_reference(bytes, b"/Root")
        .filter(|id| found.iter().any(|object| object.id == *id))
        .or_else(|| find_catalog(bytes, &found))
        .ok_or_else(|| malformed("no document catalog could be found"))?;
    let info =
        trailer_reference(bytes, b"/Info").filter(|id| found.iter().any(|object| object.id == *id));

    let mut rebuilt = bytes.to_vec();
    rebuilt.push(b'\n');
    let xref_start = rebuilt.len();
    let max_number = found.iter().map(|object| object.id.0).max().unwrap_or(0);
    rebuilt.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    for object in &found {
        rebuilt.extend_from_slice(
            format!(
                "{} 1\n{:010} {:05} n \n",
                object.id.0, object.offset, object.id.1
            )
            .as_bytes(),
        );
    }
    let mut trailer = format!(
        "trailer\n<< /Size {} /Root {} {} R",
        max_number + 1,
        root.0,
        root.1
    );
    if let Some(info) = info {
        trailer.push_str(&format!(" /Info {} {} R", info.0, info.1));
    }
    trailer.push_str(&format!(" >>\nstartxref\n{xref_start}\n%%EOF\n"));
    rebuilt.extend_from_slice(trailer.as_bytes());

    let doc = LopdfDocument::load_mem(&rebuilt).map_err(|source| PdfError::Malformed {
        path: input.to_path_buf(),
        reason: format!("recovery failed: {source}"),
    })?;

    report.recovered_objects += found.len();
    report.warnings.push(format!(
        "cross-reference table was unreadable; rebuilt it from {} objects found by scanning",
        found.len()
    ));
    Ok(doc)
}

/// Check a document that loaded cleanly for objects the table lost.
///
/// A table with a wrong offset can load without complaint and simply lack the
/// object it pointed at. When the scan finds objects the loaded document does
/// not have, rebuild from the scan, but keep the rebuilt copy only if it has
/// more pages: objects that were deleted by a later update also show up as
/// missing, and rebuilding for them would gain nothing.
fn rescue_lost_pages(
    doc: LopdfDocument,
    bytes: &[u8],
    input: &Path,
    report: &mut RepairReport,
) -> LopdfDocument {
    let lost = scan_objects(bytes)
        .iter()
        .any(|object| !doc.objects.contains_key(&object.id));
    if !lost {
        return doc;
    }

    let mut rescue_report = RepairReport::default();
    match recover_document(bytes, input, &mut rescue_report) {
        Ok(rebuilt) if rebuilt.get_pages().len() > doc.get_pages().len() => {
            report.recovered_objects += rescue_report.recovered_objects;
            report.warnings.extend(rescue_report.warnings);
            rebuilt
        }
        _ => doc,
    }
}

/// An object header found by scanning the raw file.
struct FoundObject {
    id: lopdf::ObjectId,
    offset: usize,
    /// Just past the closing `endobj`, or the end of the file if it is missing.
    end: usize,
}

/// Find every `N G obj` header, in file order, including repeats.
fn scan_headers(bytes: &[u8]) -> Vec<FoundObject> {
    let mut found = Vec::new();
    let mut position = 0;

    while position < bytes.len() {
        let at_boundary = position == 0 || bytes[position - 1].is_ascii_whitespace();
        match at_boundary
            .then(|| parse_object_header(bytes, position))
            .flatten()
        {
            Some((id, body_start)) => {
                // Skip the body, so that binary stream data is not scanned
                // for things that merely look like headers.
                let end = find_from(bytes, b"endobj", body_start)
                    .map(|end| end + b"endobj".len())
                    .unwrap_or(bytes.len());
                found.push(FoundObject {
                    id,
                    offset: position,
                    end,
                });
                position = end;
            }
            None => position += 1,
        }
    }

    found
}

/// The objects in the file, one per object number. Where a number appears
/// more than once the last one wins, as it would after an incremental update.
fn scan_objects(bytes: &[u8]) -> Vec<FoundObject> {
    let by_number: std::collections::BTreeMap<u32, FoundObject> = scan_headers(bytes)
        .into_iter()
        .map(|object| (object.id.0, object))
        .collect();
    by_number.into_values().collect()
}

/// Correct every stream whose direct `/Length` disagrees with where the data
/// actually ends.
///
/// Returns the patched bytes and the number of streams fixed, or `None` when
/// every length is right (or cannot be judged, as with an indirect length).
/// The cross-reference table of the result is stale and must be rebuilt.
fn patch_stream_lengths(bytes: &[u8]) -> Option<(Vec<u8>, usize)> {
    if find_last(bytes, b"/Encrypt").is_some() {
        return None;
    }

    // (start of replaced range, end of replaced range, replacement)
    let mut edits: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    let mut fixed = 0;

    for object in scan_headers(bytes) {
        let body = &bytes[object.offset..object.end];
        let Some(keyword) = find_stream_keyword(body) else {
            continue;
        };
        let Some((digits_start, digits_end, declared)) = direct_length(&body[..keyword]) else {
            continue;
        };

        let mut data_start = keyword + b"stream".len();
        if body[data_start..].starts_with(b"\r\n") {
            data_start += 2;
        } else if body[data_start..].starts_with(b"\n") || body[data_start..].starts_with(b"\r") {
            data_start += 1;
        }

        // A length that lands exactly on `endstream` is right.
        let declared_end = data_start.saturating_add(declared);
        if declared_end <= body.len() && ends_stream_at(&body[declared_end..]) {
            continue;
        }

        let base = object.offset;
        let (data_end, closing) = match find_from(body, b"endstream", data_start) {
            Some(marker) => (trim_one_eol(body, data_start, marker), None),
            None => {
                // The file ends mid-stream: close the stream and object.
                let end = trim_trailing_whitespace_after(body, data_start);
                (end, Some(b"\nendstream\nendobj\n".to_vec()))
            }
        };

        edits.push((
            base + digits_start,
            base + digits_end,
            (data_end - data_start).to_string().into_bytes(),
        ));
        if let Some(closing) = closing {
            edits.push((base + data_end, base + body.len(), closing));
        }
        fixed += 1;
    }

    if edits.is_empty() {
        return None;
    }

    edits.sort_by_key(|edit| edit.0);
    let mut out = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    for (start, end, replacement) in edits {
        out.extend_from_slice(&bytes[cursor..start]);
        out.extend_from_slice(&replacement);
        cursor = end;
    }
    out.extend_from_slice(&bytes[cursor..]);
    Some((out, fixed))
}

/// Position of the `stream` keyword that opens the data, not `endstream`.
fn find_stream_keyword(body: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = find_from(body, b"stream", from) {
        let after = body.get(at + b"stream".len()).copied();
        let before_ok = at == 0 || !body[at - 1].is_ascii_alphabetic();
        let after_ok = matches!(after, Some(b'\r' | b'\n'));
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// The digits of a direct `/Length N` in a stream dictionary: where they
/// start and end, and their value. An indirect length (`N G R`) is not read.
fn direct_length(dictionary: &[u8]) -> Option<(usize, usize, usize)> {
    let key = find_last(dictionary, b"/Length")?;
    let start = skip_optional_whitespace(dictionary, key + b"/Length".len());
    let (value, end) = parse_number(dictionary, start)?;
    // `/Length 12 0 R` is a reference.
    if let Some(next) = skip_whitespace(dictionary, end) {
        if let Some((_, after_generation)) = parse_number(dictionary, next) {
            let after = skip_optional_whitespace(dictionary, after_generation);
            if dictionary.get(after) == Some(&b'R') {
                return None;
            }
        }
    }
    Some((start, end, usize::try_from(value).ok()?))
}

/// Whether `rest` is an optional line ending and then `endstream`.
fn ends_stream_at(rest: &[u8]) -> bool {
    let skipped = rest
        .iter()
        .take_while(|byte| matches!(byte, b'\r' | b'\n' | b' '))
        .count();
    rest[skipped..].starts_with(b"endstream")
}

/// The end of stream data that stops at `marker`, without the line ending
/// that belongs to the `endstream` line.
fn trim_one_eol(body: &[u8], data_start: usize, marker: usize) -> usize {
    let mut end = marker;
    if end > data_start && body[end - 1] == b'\n' {
        end -= 1;
    }
    if end > data_start && body[end - 1] == b'\r' {
        end -= 1;
    }
    end
}

/// The end of stream data in an object that has no `endstream`: everything up
/// to a closing `endobj`, if there is one, less trailing whitespace.
fn trim_trailing_whitespace_after(body: &[u8], data_start: usize) -> usize {
    let mut end = body.len();
    if body.ends_with(b"endobj") {
        end -= b"endobj".len();
    }
    while end > data_start && body[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

/// Parse `<number> <generation> obj` at `position`, returning the id and the
/// index just past the keyword.
fn parse_object_header(bytes: &[u8], position: usize) -> Option<(lopdf::ObjectId, usize)> {
    let (number, next) = parse_number(bytes, position)?;
    let next = skip_whitespace(bytes, next)?;
    let (generation, next) = parse_number(bytes, next)?;
    let next = skip_whitespace(bytes, next)?;
    if !bytes[next..].starts_with(b"obj") {
        return None;
    }
    let end = next + 3;
    // "object" and friends are not headers.
    if bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    Some((
        (u32::try_from(number).ok()?, u16::try_from(generation).ok()?),
        end,
    ))
}

fn parse_number(bytes: &[u8], start: usize) -> Option<(u64, usize)> {
    let digits = bytes[start.min(bytes.len())..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 || digits > 10 {
        return None;
    }
    let value = std::str::from_utf8(&bytes[start..start + digits])
        .ok()?
        .parse()
        .ok()?;
    Some((value, start + digits))
}

/// Skip at least one whitespace byte.
fn skip_whitespace(bytes: &[u8], start: usize) -> Option<usize> {
    let count = bytes
        .get(start..)?
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    (count > 0).then_some(start + count)
}

fn find_from(bytes: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|index| index + from)
}

fn find_last(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// Read an indirect reference stored under `key` in the last trailer
/// dictionary in the file.
fn trailer_reference(bytes: &[u8], key: &[u8]) -> Option<lopdf::ObjectId> {
    let trailer = find_last(bytes, b"trailer")?;
    let key_at = find_from(bytes, key, trailer)?;
    let (number, next) = parse_number(bytes, skip_optional_whitespace(bytes, key_at + key.len()))?;
    let next = skip_whitespace(bytes, next)?;
    let (generation, next) = parse_number(bytes, next)?;
    let next = skip_whitespace(bytes, next)?;
    (bytes.get(next) == Some(&b'R'))
        .then_some((u32::try_from(number).ok()?, u16::try_from(generation).ok()?))
}

/// Skip zero or more whitespace bytes.
fn skip_optional_whitespace(bytes: &[u8], start: usize) -> usize {
    start
        + bytes.get(start..).map_or(0, |rest| {
            rest.iter()
                .take_while(|byte| byte.is_ascii_whitespace())
                .count()
        })
}

/// Find an object that declares itself the document catalog.
fn find_catalog(bytes: &[u8], found: &[FoundObject]) -> Option<lopdf::ObjectId> {
    let mut ends: Vec<usize> = found.iter().map(|object| object.offset).skip(1).collect();
    ends.push(bytes.len());
    found.iter().zip(ends).find_map(|(object, end)| {
        let body = &bytes[object.offset..end];
        let compact: Vec<u8> = body
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        let is_catalog = compact
            .windows(b"/Type/Catalog".len())
            .any(|w| w == b"/Type/Catalog");
        is_catalog.then_some(object.id)
    })
}

/// Fix stream /Length entries that don't match the actual stream data.
fn fix_stream_lengths(doc: &mut LopdfDocument) -> usize {
    let mut fixed = 0;

    // First pass: collect stream object IDs and their actual lengths.
    let mut stream_info: Vec<(lopdf::ObjectId, usize, Option<lopdf::ObjectId>)> = Vec::new();
    for (id, object) in doc.objects.iter() {
        if let Object::Stream(stream) = object {
            let actual_len = stream.content.len();
            let length_ref = stream
                .dict
                .get(b"Length")
                .ok()
                .and_then(|o| o.as_reference().ok());
            stream_info.push((*id, actual_len, length_ref));
        }
    }

    // Second pass: fix the lengths that don't match.
    for (id, actual_len, length_ref) in stream_info {
        let needs_fix = match length_ref {
            Some(ref_id) => {
                // Length is an indirect reference; resolve it.
                match doc.objects.get(&ref_id) {
                    Some(obj) => match obj.as_i64() {
                        Ok(declared) => (declared as usize) != actual_len,
                        Err(_) => true, // Not an integer, assume mismatch
                    },
                    None => true, // Referenced object doesn't exist
                }
            }
            None => {
                // Check if there's a direct integer Length.
                let has_mismatch = match doc.objects.get(&id).and_then(|o| o.as_stream().ok()) {
                    Some(stream) => match stream.dict.get(b"Length").ok() {
                        Some(Object::Integer(declared)) => (*declared as usize) != actual_len,
                        _ => true, // No Length entry at all or not an integer
                    },
                    None => true,
                };
                has_mismatch
            }
        };

        if needs_fix {
            if let Some(stream) = doc
                .objects
                .get_mut(&id)
                .and_then(|o| o.as_stream_mut().ok())
            {
                stream
                    .dict
                    .set("Length", Object::Integer(actual_len as i64));
                fixed += 1;
            }
        }
    }

    fixed
}

/// Ensure the trailer has required entries (Root, Size, etc.).
fn fix_trailer(doc: &mut LopdfDocument, report: &mut RepairReport) {
    // Ensure Root points to a Catalog.
    if doc.trailer.get(b"Root").is_err() {
        // Find a Catalog object.
        if let Some((id, _)) = doc
            .objects
            .iter()
            .find(|(_, o)| o.type_name().map(|n| n == b"Catalog").unwrap_or(false))
        {
            doc.trailer.set("Root", Object::Reference(*id));
            report
                .warnings
                .push("trailer Root was missing; reconstructed from Catalog".into());
        } else {
            report
                .warnings
                .push("no Catalog found; document may be unusable".into());
        }
    }

    // Size always follows the object numbers after renumbering, so setting it
    // is bookkeeping rather than a repair worth reporting.
    doc.trailer
        .set("Size", Object::Integer(i64::from(doc.max_id) + 1));
}

/// Repair a document that is already open, in place.
///
/// The in-memory variant of [`repair`]: it fixes stream lengths, drops
/// unreachable objects and corrects the trailer, but cannot recover from a
/// file that failed to load, since there is nothing to repair yet.
pub fn repair_loaded(doc: &mut Document) -> RepairReport {
    let mut report = RepairReport::default();
    repair_inner(&mut doc.inner, &mut report);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Build a PDF from object bodies (object 1 first), with a correct
    /// cross-reference table.
    fn build_pdf(bodies: &[&str]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in bodies.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                bodies.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
    const PAGES: &str = "<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
    const PAGE: &str = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>";

    fn run_repair(bytes: &[u8]) -> (Result<RepairReport>, NamedTempFile) {
        let mut input = NamedTempFile::new().unwrap();
        input.write_all(bytes).unwrap();
        input.flush().unwrap();
        let output = NamedTempFile::new().unwrap();
        (repair(input.path(), output.path()), output)
    }

    #[test]
    fn repair_clean_pdf_works() {
        let (report, output) = run_repair(&build_pdf(&[CATALOG, PAGES, PAGE]));
        // A clean PDF should need no repairs.
        assert!(report.unwrap().is_clean());
        assert_eq!(Document::open(output.path()).unwrap().page_count(), 1);
    }

    #[test]
    fn repair_truncated_stream() {
        // The declared length is larger than the data that follows.
        let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>";
        let pdf = build_pdf(&[
            CATALOG,
            PAGES,
            page,
            "<< /Length 100 >>\nstream\nshort\nendstream",
        ]);
        let (report, output) = run_repair(&pdf);
        assert_eq!(report.unwrap().fixed_streams, 1);
        assert_eq!(Document::open(output.path()).unwrap().page_count(), 1);
    }

    #[test]
    fn repair_survives_bad_xref_offsets() {
        let mut pdf = build_pdf(&[CATALOG, PAGES, PAGE]);
        // Point object 2 somewhere it is not.
        let text = String::from_utf8(pdf.clone()).unwrap();
        let xref = text.find("xref\n").unwrap();
        let entries = text[xref..].find("0000000009").unwrap() + xref;
        let second = text[entries..].find('\n').unwrap() + entries + 1;
        let second = text[second..].find('\n').unwrap() + second + 1;
        pdf[second..second + 10].copy_from_slice(b"0000009999");

        let (report, output) = run_repair(&pdf);
        let report = report.unwrap();
        assert!(report.recovered_objects >= 3, "{report:?}");
        assert_eq!(Document::open(output.path()).unwrap().page_count(), 1);
    }

    const STREAM_PAGE: &str =
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>";

    #[test]
    fn repair_leaves_a_correct_stream_alone() {
        let pdf = build_pdf(&[
            CATALOG,
            PAGES,
            STREAM_PAGE,
            "<< /Length 5 >>\nstream\nhello\nendstream",
        ]);
        assert!(patch_stream_lengths(&pdf).is_none());
        let (report, _) = run_repair(&pdf);
        assert!(report.unwrap().is_clean());
    }

    #[test]
    fn repair_stream_that_is_too_short_keeps_all_its_data() {
        let pdf = build_pdf(&[
            CATALOG,
            PAGES,
            STREAM_PAGE,
            "<< /Length 2 >>\nstream\nhello\nendstream",
        ]);
        let (patched, count) = patch_stream_lengths(&pdf).unwrap();
        assert_eq!(count, 1);
        let text = String::from_utf8(patched).unwrap();
        assert!(
            text.contains("/Length 5 >>\nstream\nhello\nendstream"),
            "{text}"
        );
    }

    #[test]
    fn repair_file_cut_off_inside_a_stream() {
        let pdf = build_pdf(&[
            CATALOG,
            PAGES,
            STREAM_PAGE,
            "<< /Length 500 >>\nstream\nq 1 0 0 1 0 0 cm Q\nendstream",
        ]);
        let cut = pdf.windows(9).position(|w| w == b"endstream").unwrap();
        let (report, output) = run_repair(&pdf[..cut]);
        let report = report.unwrap();
        assert_eq!(report.fixed_streams, 1);

        let doc = Document::open(output.path()).unwrap();
        assert_eq!(doc.page_count(), 1);
        let page_id = doc.inner.get_pages().values().next().copied().unwrap();
        let contents = doc.inner.get_page_content(page_id);
        assert_eq!(contents.trim_ascii_end(), b"q 1 0 0 1 0 0 cm Q");
    }

    #[test]
    fn repair_loaded_fixes_a_wrong_length_in_memory() {
        let mut doc = Document::from_lopdf(
            {
                let mut inner =
                    LopdfDocument::load_mem(&build_pdf(&[CATALOG, PAGES, PAGE])).unwrap();
                let stream = lopdf::Stream::new(lopdf::Dictionary::new(), b"abc".to_vec());
                let id = inner.add_object(stream);
                inner
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .as_stream_mut()
                    .unwrap()
                    .dict
                    .set("Length", 99);
                // Keep it reachable so pruning leaves it alone.
                let page_id = inner.get_pages().values().next().copied().unwrap();
                inner
                    .get_dictionary_mut(page_id)
                    .unwrap()
                    .set("Contents", Object::Reference(id));
                inner
            },
            None,
        );
        let report = repair_loaded(&mut doc);
        assert_eq!(report.fixed_streams, 1);
        assert_eq!(doc.page_count(), 1);
    }

    #[test]
    fn repair_missing_trailer_finds_the_catalog() {
        let pdf = build_pdf(&[CATALOG, PAGES, PAGE]);
        let cut = pdf.windows(4).rposition(|w| w == b"xref").unwrap();
        let (report, output) = run_repair(&pdf[..cut]);
        assert!(report.unwrap().recovered_objects >= 3);
        assert_eq!(Document::open(output.path()).unwrap().page_count(), 1);
    }

    #[test]
    fn repair_rejects_files_with_no_objects() {
        let (report, _) = run_repair(b"this is not a PDF at all");
        assert!(matches!(report, Err(PdfError::Malformed { .. })));
    }

    #[test]
    fn repair_rejects_files_with_no_catalog() {
        let (report, _) = run_repair(b"%PDF-1.4\n1 0 obj\n<< /Foo /Bar >>\nendobj\n");
        assert!(matches!(report, Err(PdfError::Malformed { .. })));
    }

    #[test]
    fn scan_takes_the_last_definition_of_an_object() {
        let bytes = b"1 0 obj\n(old)\nendobj\n1 0 obj\n(new)\nendobj\n";
        let found = scan_objects(bytes);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].offset, 21);
    }

    #[test]
    fn scan_ignores_header_lookalikes_inside_streams() {
        let bytes = b"1 0 obj\n<< /Length 20 >>\nstream\n2 0 obj 3 0 obj\nendstream\nendobj\n";
        let found = scan_objects(bytes);
        assert_eq!(found.len(), 1);
    }
}
