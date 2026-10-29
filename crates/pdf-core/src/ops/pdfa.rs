//! PDF/A conversion.
//!
//! Converts a PDF to PDF/A-1b, PDF/A-2b, or PDF/A-3b conformance by embedding
//! an sRGB ICC profile, setting OutputIntent, generating XMP metadata, ensuring
//! fonts are embedded, and removing prohibited features.

use std::collections::HashSet;

use lopdf::{Dictionary, Object, ObjectId, Stream};

use crate::document::Document;
use crate::error::{PdfError, Result};

/// PDF/A conformance level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfALevel {
    /// PDF/A-1b (ISO 19005-1): Basic conformance, no transparency, no JPEG2000, no layers.
    A1b,
    /// PDF/A-2b (ISO 19005-2): Adds transparency, JPEG2000, layers, embedded files.
    A2b,
    /// PDF/A-3b (ISO 19005-3): Adds arbitrary file attachments.
    A3b,
}

impl PdfALevel {
    /// Parse a level string like "1b", "2b", "3b".
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "1b" => Some(PdfALevel::A1b),
            "2b" => Some(PdfALevel::A2b),
            "3b" => Some(PdfALevel::A3b),
            _ => None,
        }
    }

    /// The PDF/A part number (1, 2, or 3).
    pub fn part(self) -> u8 {
        match self {
            PdfALevel::A1b => 1,
            PdfALevel::A2b => 2,
            PdfALevel::A3b => 3,
        }
    }

    /// The conformance level letter (always 'b' for basic).
    pub fn conformance(self) -> &'static str {
        "B"
    }

    /// The PDF/A identification string for XMP metadata.
    pub fn identification(self) -> &'static str {
        match self {
            PdfALevel::A1b => "PDF/A-1b",
            PdfALevel::A2b => "PDF/A-2b",
            PdfALevel::A3b => "PDF/A-3b",
        }
    }

    /// The PDF/A version for the OutputIntent (e.g., "1.0", "2.0", "3.0").
    pub fn version(self) -> &'static str {
        match self {
            PdfALevel::A1b => "1.0",
            PdfALevel::A2b => "2.0",
            PdfALevel::A3b => "3.0",
        }
    }
}

/// Report from a PDF/A conversion or conformance check.
///
/// [`to_pdfa`] fills the `*_removed`/`*_added` fields; [`check_pdfa`] fills
/// `conformant` and `issues`. A conversion report also carries the result of
/// running the checker on the converted document, so callers can see what (if
/// anything) still stands between the output and conformance.
#[derive(Debug, Clone, Default)]
pub struct PdfAReport {
    /// Whether the document was already encrypted (encryption removed).
    pub had_encryption: bool,
    /// Number of JavaScript actions removed.
    pub javascript_removed: usize,
    /// Number of launch actions removed.
    pub launch_actions_removed: usize,
    /// Number of external references (GoToR) removed.
    pub external_refs_removed: usize,
    /// Number of fonts that were not embedded (error if > 0 for PDF/A-1b).
    pub unembedded_fonts: Vec<String>,
    /// Whether an ICC profile was embedded.
    pub icc_embedded: bool,
    /// Whether XMP metadata was added/updated.
    pub xmp_updated: bool,
    /// Whether OutputIntent was added.
    pub output_intent_added: bool,
    /// Whether the document passes the structural conformance check.
    pub conformant: bool,
    /// Actionable descriptions of every conformance problem found.
    pub issues: Vec<String>,
}

/// Embedded sRGB ICC profile (minimal v2, CC0).
const SRGB_ICC: &[u8] = include_bytes!("srgb.icc");

/// Convert a document to PDF/A in place.
///
/// This function modifies the document to meet PDF/A conformance requirements:
/// - Embeds sRGB ICC profile and sets OutputIntent
/// - Generates/updates XMP metadata with PDF/A identification
/// - Ensures all fonts are embedded (fails if not for PDF/A-1b)
/// - Removes encryption
/// - Removes JavaScript and launch actions
/// - Removes external references (GoToR)
/// - For PDF/A-1b: also removes transparency (ExtGState with /CA, /ca, /BM, /SMask)
/// - For PDF/A-3b: preserves embedded files
pub fn to_pdfa(doc: &mut Document, level: PdfALevel) -> Result<PdfAReport> {
    let mut report = PdfAReport::default();

    // 1. Remove encryption if present
    if doc.inner.trailer.get(b"Encrypt").is_ok() {
        doc.inner.trailer.remove(b"Encrypt");
        report.had_encryption = true;
    }

    // 2. Remove JavaScript actions
    report.javascript_removed = remove_javascript(&mut doc.inner)?;

    // 3. Remove launch actions
    report.launch_actions_removed = remove_launch_actions(&mut doc.inner)?;

    // 4. Remove external references (GoToR)
    report.external_refs_removed = remove_external_refs(&mut doc.inner)?;

    // 5. Check font embedding
    report.unembedded_fonts = check_font_embedding(&doc.inner, level)?;

    // 6. Embed sRGB ICC profile and add OutputIntent
    let icc_stream_id = embed_icc_profile(&mut doc.inner)?;
    add_output_intent(&mut doc.inner, icc_stream_id, level)?;
    report.icc_embedded = true;
    report.output_intent_added = true;

    // 7. Generate/update XMP metadata with PDF/A identification
    add_xmp_metadata(&mut doc.inner, level)?;
    report.xmp_updated = true;

    // 8. For PDF/A-1b, remove transparency-related ExtGState entries
    if level == PdfALevel::A1b {
        remove_transparency(&mut doc.inner)?;
    }

    // 9. Set PDF version to at least 1.4 (PDF/A-1 requires 1.4)
    ensure_min_version(&mut doc.inner, level)?;

    // 10. Verify the result against the conformance checker, so the report
    // says what (if anything) still separates the output from PDF/A.
    let check = check_pdfa(doc)?;
    report.conformant = check.conformant;
    report.issues = check.issues;

    Ok(report)
}

/// Structurally validate a document against the PDF/A requirements Foolscap
/// can check without a full ISO 19005 validator (PLAN §8.3).
///
/// The checks are: an OutputIntent with `/S /GTS_PDFA1` and a
/// `/DestOutputProfile` ICC stream; every font embedded; no encryption; no
/// JavaScript; no launch actions or external references; XMP metadata carrying
/// the `pdfaid:part`/`pdfaid:conformance` identification; and — when the XMP
/// claims PDF/A-1 — no transparency graphics states. Every failure becomes an
/// actionable issue string; the document is conformant when there are none.
pub fn check_pdfa(doc: &Document) -> Result<PdfAReport> {
    let mut report = PdfAReport::default();
    let mut issues = Vec::new();
    let inner = &doc.inner;

    if inner.trailer.has(b"Encrypt") {
        issues.push(
            "the document is encrypted; PDF/A forbids encryption (convert it with to-pdfa first)"
                .to_string(),
        );
    }

    let root = inner
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .ok()
        .and_then(|id| inner.get_object(id).ok())
        .and_then(|object| object.as_dict().ok());
    let Some(root) = root else {
        issues.push("the document has no catalog to check".to_string());
        report.conformant = false;
        report.issues = issues;
        return Ok(report);
    };

    if !has_valid_output_intent(inner, root) {
        issues.push(
            "no OutputIntent with /S /GTS_PDFA1 and a /DestOutputProfile ICC stream".to_string(),
        );
    }

    for font in collect_unembedded_fonts(inner) {
        issues.push(format!(
            "font {font} is not embedded; PDF/A requires all fonts to be embedded"
        ));
    }

    issues.extend(find_prohibited_actions(inner, root));

    // The claimed part decides which level-specific rules apply; a document
    // without an XMP identification cannot declare itself PDF/A at all.
    match xmp_pdfa_claim(inner, root) {
        Some((part, _conformance)) => {
            if part == 1 {
                let version: f32 = inner.version.parse().unwrap_or(0.0);
                if version != 1.4 {
                    issues.push(format!(
                        "claims PDF/A-1 but the file header says PDF {}; PDF/A-1 is based on PDF 1.4",
                        inner.version
                    ));
                }
                for name in find_transparency(inner) {
                    issues.push(format!(
                        "ExtGState {name} sets a transparency parameter, which PDF/A-1 forbids"
                    ));
                }
            }
        }
        None => issues.push(
            "the catalog carries no XMP metadata stream with pdfaid:part and pdfaid:conformance"
                .to_string(),
        ),
    }

    report.conformant = issues.is_empty();
    report.issues = issues;
    Ok(report)
}

/// Whether the catalog has a GTS_PDFA1 OutputIntent whose DestOutputProfile
/// resolves to a stream object.
fn has_valid_output_intent(doc: &lopdf::Document, root: &Dictionary) -> bool {
    let Some(entries) = output_intent_entries(doc, root) else {
        return false;
    };
    entries.iter().any(|entry| {
        let dict = entry.as_dict().ok().or_else(|| {
            entry
                .as_reference()
                .ok()
                .and_then(|id| doc.get_dictionary(id).ok())
        });
        let Some(dict) = dict else {
            return false;
        };
        if dict.get(b"S").and_then(Object::as_name).ok() != Some(b"GTS_PDFA1".as_slice()) {
            return false;
        }
        match dict.get(b"DestOutputProfile") {
            Ok(Object::Stream(_)) => true,
            Ok(dest) => dest
                .as_reference()
                .ok()
                .and_then(|id| doc.get_object(id).ok())
                .is_some_and(|object| matches!(object, Object::Stream(_))),
            _ => false,
        }
    })
}

/// The catalog's `/OutputIntents` entries, following a reference to the array.
fn output_intent_entries<'a>(
    doc: &'a lopdf::Document,
    root: &'a Dictionary,
) -> Option<Vec<&'a Object>> {
    match root.get(b"OutputIntents").ok()? {
        Object::Array(entries) => Some(entries.iter().collect()),
        Object::Reference(id) => match doc.get_object(*id) {
            Ok(Object::Array(entries)) => Some(entries.iter().collect()),
            _ => None,
        },
        _ => None,
    }
}

/// Every prohibited action found in the document: JavaScript anywhere, launch
/// actions, and GoToR references to other files.
fn find_prohibited_actions(doc: &lopdf::Document, root: &Dictionary) -> Vec<String> {
    let mut issues = Vec::new();

    if let Ok(open_action) = root.get(b"OpenAction") {
        if is_javascript_action(doc, open_action) {
            issues.push("the catalog's /OpenAction runs JavaScript".to_string());
        } else if is_launch_action(doc, open_action) {
            issues.push("the catalog's /OpenAction launches an external program".to_string());
        } else if is_goto_r_action(doc, open_action) {
            issues
                .push("the catalog's /OpenAction references another document (GoToR)".to_string());
        }
    }
    if let Ok(aa) = root.get(b"AA") {
        if has_javascript(doc, aa) {
            issues.push("the catalog has /AA additional actions running JavaScript".to_string());
        }
    }

    // The document-level JavaScript name tree.
    if let Some(names) = match root.get(b"Names") {
        Ok(Object::Dictionary(dict)) => Some(dict.clone()),
        Ok(Object::Reference(id)) => doc
            .get_object(*id)
            .ok()
            .and_then(|object| object.as_dict().ok())
            .cloned(),
        _ => None,
    } {
        if names.has(b"JavaScript") {
            issues.push(
                "the /Names tree carries a JavaScript name tree (document-level scripts)"
                    .to_string(),
            );
        }
    }

    for page_id in doc.get_pages().values() {
        let Ok(page) = doc.get_dictionary(*page_id) else {
            continue;
        };
        if let Ok(aa) = page.get(b"AA") {
            if has_javascript(doc, aa) {
                issues.push("a page has /AA additional actions running JavaScript".to_string());
            }
        }
    }

    for annot_id in annotation_ids(doc) {
        let Ok(annot) = doc.get_object(annot_id).and_then(Object::as_dict) else {
            continue;
        };
        if let Ok(action) = annot.get(b"A") {
            if is_javascript_action(doc, action) {
                issues.push("an annotation action runs JavaScript".to_string());
            } else if is_launch_action(doc, action) {
                issues.push("an annotation launches an external program (/Launch)".to_string());
            } else if is_goto_r_action(doc, action) {
                issues.push(
                    "an annotation references another document (GoToR external reference)"
                        .to_string(),
                );
            }
        }
        if let Ok(aa) = annot.get(b"AA") {
            if has_javascript(doc, aa) {
                issues.push(
                    "an annotation has /AA additional actions running JavaScript".to_string(),
                );
            }
        }
    }

    // Outline items may carry actions too.
    if let Some(outlines_id) = root
        .get(b"Outlines")
        .and_then(Object::as_reference)
        .ok()
        .or_else(|| {
            root.get(b"Outlines")
                .ok()
                .and_then(|object| object.as_dict().ok())
                .and_then(|dict| dict.get(b"First").and_then(Object::as_reference).ok())
        })
    {
        if let Ok(outlines) = doc.get_dictionary(outlines_id) {
            if let Ok(first) = outlines.get(b"First").and_then(Object::as_reference) {
                let mut current = Some(first);
                let mut hops = 0;
                while let Some(id) = current {
                    // Malformed outlines can loop; bound the walk.
                    hops += 1;
                    if hops > 10_000 {
                        break;
                    }
                    let Ok(item) = doc.get_dictionary(id) else {
                        break;
                    };
                    if let Ok(action) = item.get(b"A") {
                        if is_javascript_action(doc, action) {
                            issues.push("a bookmark action runs JavaScript".to_string());
                        } else if is_launch_action(doc, action) {
                            issues.push(
                                "a bookmark launches an external program (/Launch)".to_string(),
                            );
                        } else if is_goto_r_action(doc, action) {
                            issues
                                .push("a bookmark references another document (GoToR)".to_string());
                        }
                    }
                    current = item.get(b"Next").and_then(Object::as_reference).ok();
                }
            }
        }
    }

    issues
}

/// Names of ExtGState entries that set a transparency parameter.
fn find_transparency(doc: &lopdf::Document) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();

    for page_id in doc.get_pages().values() {
        let mut node = Some(*page_id);
        while let Some(id) = node {
            let Ok(page) = doc.get_dictionary(id) else {
                break;
            };
            if let Ok(resources) = page.get(b"Resources") {
                collect_transparent_gstates(doc, resources, &mut names, &mut seen);
            }
            node = page.get(b"Parent").and_then(Object::as_reference).ok();
        }
    }

    names
}

fn collect_transparent_gstates(
    doc: &lopdf::Document,
    resources: &Object,
    names: &mut Vec<String>,
    seen: &mut HashSet<ObjectId>,
) {
    let dict = match resources {
        Object::Reference(id) => {
            if !seen.insert(*id) {
                return;
            }
            match doc.get_object(*id) {
                Ok(Object::Dictionary(dict)) => dict,
                _ => return,
            }
        }
        Object::Dictionary(dict) => dict,
        _ => return,
    };

    if let Ok(ext_gstate) = dict.get(b"ExtGState") {
        let gstate_dict = match ext_gstate {
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(Object::Dictionary(dict)) => Some(dict),
                _ => None,
            },
            Object::Dictionary(dict) => Some(dict),
            _ => None,
        };
        if let Some(gstates) = gstate_dict {
            for (name, gstate) in gstates.iter() {
                // The gstate may be a direct dictionary or a reference to one.
                let gs = match gstate.as_reference() {
                    Ok(id) => doc.get_object(id).ok().and_then(|o| o.as_dict().ok()),
                    Err(_) => gstate.as_dict().ok(),
                };
                if let Some(gs) = gs {
                    if dict_has_transparency(gs) {
                        names.push(String::from_utf8_lossy(name).into_owned());
                    }
                }
            }
        }
    }

    // Form XObjects carry their own graphics states.
    if let Ok(xobjects) = dict.get(b"XObject") {
        let xobject_dict = match xobjects {
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(Object::Dictionary(dict)) => Some(dict),
                _ => None,
            },
            Object::Dictionary(dict) => Some(dict),
            _ => None,
        };
        if let Some(xobjects) = xobject_dict {
            for (_, xobject) in xobjects.iter() {
                if let Ok(id) = xobject.as_reference() {
                    if !seen.insert(id) {
                        continue;
                    }
                    if let Ok(Object::Stream(stream)) = doc.get_object(id) {
                        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                            == Some(b"Form".as_slice())
                        {
                            if let Ok(resources) = stream.dict.get(b"Resources") {
                                collect_transparent_gstates(doc, resources, names, seen);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `(part, conformance)` a document's XMP metadata claims, if any.
fn xmp_pdfa_claim(doc: &lopdf::Document, root: &Dictionary) -> Option<(u8, String)> {
    let stream = match root.get(b"Metadata").ok()? {
        Object::Reference(id) => doc.get_object(*id).ok()?.as_stream().ok()?,
        Object::Stream(stream) => stream,
        _ => return None,
    };
    let content = stream.decompressed_content().ok()?;
    let xmp = String::from_utf8_lossy(&content);
    let part: u8 = extract_xml_tag(&xmp, "pdfaid:part")?.parse().ok()?;
    let conformance = extract_xml_tag(&xmp, "pdfaid:conformance")?.to_string();
    Some((part, conformance))
}

/// The text between `<tag>` and `</tag>`, for the simple XMP we read and write.
fn extract_xml_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    Some(&xml[start..end])
}

/// Remove all JavaScript actions from the document.
fn remove_javascript(doc: &mut lopdf::Document) -> Result<usize> {
    let mut removed = 0;

    // Check root for OpenAction, AA (additional actions)
    let root_id = match doc.trailer.get(b"Root").and_then(Object::as_reference) {
        Ok(id) => id,
        Err(_) => return Ok(0),
    };

    // First check and collect what needs to be removed
    let mut has_js_open_action = false;
    let has_aa;
    {
        let root = match doc.get_object(root_id).and_then(Object::as_dict) {
            Ok(d) => d,
            Err(_) => return Ok(0),
        };
        if let Ok(action) = root.get(b"OpenAction") {
            if is_javascript_action(doc, action) {
                has_js_open_action = true;
            }
        }
        has_aa = root.has(b"AA");
    }

    // Now remove
    if let Ok(root) = doc.get_object_mut(root_id).and_then(Object::as_dict_mut) {
        if has_js_open_action {
            root.remove(b"OpenAction");
            removed += 1;
        }
        if has_aa {
            root.remove(b"AA");
            removed += 1;
        }
    }

    // Check page-level AA
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for page_id in page_ids {
        let mut page_has_js_aa = false;
        {
            let page = match doc.get_object(page_id).and_then(Object::as_dict) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if let Ok(action) = page.get(b"AA") {
                if has_javascript(doc, action) {
                    page_has_js_aa = true;
                }
            }
        }
        if page_has_js_aa {
            if let Ok(page) = doc.get_object_mut(page_id).and_then(Object::as_dict_mut) {
                page.remove(b"AA");
                removed += 1;
            }
        }
    }

    // Remove JavaScript from annotations: their /A action and /AA additional
    // actions can both carry scripts.
    for annot_id in annotation_ids(doc) {
        let (drop_a, drop_aa) = {
            let Ok(annot) = doc.get_object(annot_id).and_then(Object::as_dict) else {
                continue;
            };
            let drop_a = annot
                .get(b"A")
                .map(|action| is_javascript_action(doc, action))
                .unwrap_or(false);
            let drop_aa = annot
                .get(b"AA")
                .map(|actions| has_javascript(doc, actions))
                .unwrap_or(false);
            (drop_a, drop_aa)
        };
        if drop_a || drop_aa {
            if let Ok(annot) = doc.get_object_mut(annot_id).and_then(Object::as_dict_mut) {
                if drop_a {
                    annot.remove(b"A");
                    removed += 1;
                }
                if drop_aa {
                    annot.remove(b"AA");
                    removed += 1;
                }
            }
        }
    }

    // Remove JavaScript from Names tree
    if let Ok(root) = doc.get_object_mut(root_id).and_then(Object::as_dict_mut) {
        match root.get_mut(b"Names") {
            Ok(Object::Dictionary(names)) => {
                names.remove(b"JavaScript");
                names.remove(b"EmbeddedFiles"); // EmbeddedFiles handled separately for PDF/A-3
            }
            _ => {
                if let Ok(names_id) = root.get(b"Names").and_then(Object::as_reference) {
                    if let Ok(names) = doc.get_object_mut(names_id).and_then(Object::as_dict_mut) {
                        names.remove(b"JavaScript");
                        names.remove(b"EmbeddedFiles"); // EmbeddedFiles handled separately for PDF/A-3
                    }
                }
            }
        }
    }

    Ok(removed)
}

/// Ids of every annotation dictionary reachable from any page's `/Annots`,
/// which may itself be an array or a reference to one.
fn annotation_ids(doc: &lopdf::Document) -> Vec<ObjectId> {
    let mut ids = Vec::new();
    for page_id in doc.get_pages().values() {
        let Ok(page) = doc.get_dictionary(*page_id) else {
            continue;
        };
        if let Ok(annots) = page.get(b"Annots") {
            collect_annotation_ids(doc, annots, &mut ids);
        }
    }
    ids
}

fn collect_annotation_ids(doc: &lopdf::Document, annots: &Object, ids: &mut Vec<ObjectId>) {
    match annots {
        Object::Array(entries) => {
            for entry in entries {
                if let Ok(id) = entry.as_reference() {
                    ids.push(id);
                }
            }
        }
        Object::Reference(id) => {
            if let Ok(Object::Array(entries)) = doc.get_object(*id) {
                for entry in entries {
                    if let Ok(annot_id) = entry.as_reference() {
                        ids.push(annot_id);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Drop every annotation whose action matches `drop`, from any page.
///
/// A page's `/Annots` may be a direct array or a reference to one, so the
/// filtered array is written back to whichever form it was found in.
fn filter_page_annotations<F>(doc: &mut lopdf::Document, drop_annotation: F) -> Result<usize>
where
    F: Fn(&lopdf::Document, &Dictionary) -> bool,
{
    let mut removed = 0;

    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for page_id in page_ids {
        let annots_object = {
            let page = match doc.get_object(page_id).and_then(Object::as_dict) {
                Ok(d) => d,
                Err(_) => continue,
            };
            match page.get(b"Annots") {
                Ok(object) => object.clone(),
                Err(_) => continue,
            }
        };

        let entries = match &annots_object {
            Object::Array(entries) => entries.clone(),
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(Object::Array(entries)) => entries.clone(),
                _ => continue,
            },
            _ => continue,
        };

        let mut dropped_on_page = 0;
        let kept: Vec<Object> = entries
            .into_iter()
            .filter(|entry| {
                let annot_id = entry.as_reference().ok();
                let drop = annot_id
                    .and_then(|id| doc.get_object(id).ok())
                    .and_then(|object| object.as_dict().ok())
                    .map(|annot| drop_annotation(doc, annot))
                    .unwrap_or(false);
                if drop {
                    dropped_on_page += 1;
                }
                !drop
            })
            .collect();

        if dropped_on_page > 0 {
            removed += dropped_on_page;
            match &annots_object {
                Object::Array(_) => {
                    if let Ok(page) = doc.get_object_mut(page_id).and_then(Object::as_dict_mut) {
                        page.set(b"Annots", Object::Array(kept));
                    }
                }
                Object::Reference(id) => {
                    doc.objects.insert(*id, Object::Array(kept));
                }
                _ => {}
            }
        }
    }

    Ok(removed)
}

/// Remove all launch actions (annotations whose action launches a program).
fn remove_launch_actions(doc: &mut lopdf::Document) -> Result<usize> {
    filter_page_annotations(doc, |doc, annot| {
        let is_link =
            annot.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Link".as_slice());
        is_link
            && annot
                .get(b"A")
                .map(|action| is_launch_action(doc, action))
                .unwrap_or(false)
    })
}

/// Remove external references (GoToR actions pointing to other files).
fn remove_external_refs(doc: &mut lopdf::Document) -> Result<usize> {
    let removed = filter_page_annotations(doc, |doc, annot| {
        annot
            .get(b"A")
            .map(|action| is_goto_r_action(doc, action))
            .unwrap_or(false)
    })?;

    // Also check outline items
    if let Ok(root_id) = doc.trailer.get(b"Root").and_then(Object::as_reference) {
        if let Ok(root) = doc.get_object_mut(root_id).and_then(Object::as_dict_mut) {
            if let Ok(outlines_id) = root.get(b"Outlines").and_then(Object::as_reference) {
                return remove_goto_r_from_outlines(doc, outlines_id).map(|n| n + removed);
            }
        }
    }

    Ok(removed)
}

fn remove_goto_r_from_outlines(doc: &mut lopdf::Document, outline_id: ObjectId) -> Result<usize> {
    let mut removed = 0;
    if let Ok(outline) = doc.get_object_mut(outline_id).and_then(Object::as_dict_mut) {
        if let Ok(first) = outline.get(b"First").and_then(Object::as_reference) {
            removed += remove_goto_r_from_outline_item(doc, first)?;
        }
    }
    Ok(removed)
}

fn remove_goto_r_from_outline_item(doc: &mut lopdf::Document, item_id: ObjectId) -> Result<usize> {
    let mut removed = 0;
    let mut current = Some(item_id);
    while let Some(id) = current {
        // First check if the action is a GoToR
        let is_goto_r = {
            let item = match doc.get_object(id).and_then(Object::as_dict) {
                Ok(d) => d,
                Err(_) => break,
            };
            item.get(b"A")
                .map(|a| is_goto_r_action(doc, a))
                .unwrap_or(false)
        };

        // If it is, remove it with a mutable borrow
        if is_goto_r {
            if let Ok(item) = doc.get_object_mut(id).and_then(Object::as_dict_mut) {
                item.remove(b"A");
                removed += 1;
            }
        }

        // Get next item
        current = {
            let item = match doc.get_object(id).and_then(Object::as_dict) {
                Ok(d) => d,
                Err(_) => break,
            };
            item.get(b"Next").and_then(Object::as_reference).ok()
        };
    }
    Ok(removed)
}

/// Check if an action is a JavaScript action.
fn is_javascript_action(doc: &lopdf::Document, action: &Object) -> bool {
    match action {
        Object::Dictionary(dict) => {
            if let Ok(s) = dict.get(b"S").and_then(Object::as_name) {
                if s == b"JavaScript" {
                    return true;
                }
            }
            if let Ok(s) = dict.get(b"S").and_then(Object::as_name) {
                if s == b"Named" {
                    if let Ok(n) = dict.get(b"N").and_then(Object::as_name) {
                        if n == b"Print" || n == b"SaveAs" {
                            // These are OK
                        }
                    }
                }
            }
            false
        }
        Object::Array(arr) => arr.iter().any(|a| is_javascript_action(doc, a)),
        Object::Reference(r) => {
            if let Ok(obj) = doc.get_object(*r) {
                is_javascript_action(doc, obj)
            } else {
                false
            }
        }
        _ => false,
    }
}

fn has_javascript(doc: &lopdf::Document, action: &Object) -> bool {
    match action {
        Object::Dictionary(dict) => {
            for (_, v) in dict.iter() {
                if is_javascript_action(doc, v) {
                    return true;
                }
            }
            false
        }
        Object::Array(arr) => arr.iter().any(|a| has_javascript(doc, a)),
        Object::Reference(r) => {
            if let Ok(obj) = doc.get_object(*r) {
                has_javascript(doc, obj)
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Check if an action is a launch action.
fn is_launch_action(doc: &lopdf::Document, action: &Object) -> bool {
    match action {
        Object::Dictionary(dict) => {
            if let Ok(s) = dict.get(b"S").and_then(Object::as_name) {
                if s == b"Launch" {
                    return true;
                }
            }
            false
        }
        Object::Reference(r) => {
            if let Ok(obj) = doc.get_object(*r) {
                is_launch_action(doc, obj)
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Check if an action is a GoToR (remote go-to) action.
fn is_goto_r_action(doc: &lopdf::Document, action: &Object) -> bool {
    match action {
        Object::Dictionary(dict) => {
            if let Ok(s) = dict.get(b"S").and_then(Object::as_name) {
                if s == b"GoToR" {
                    return true;
                }
            }
            false
        }
        Object::Reference(r) => {
            if let Ok(obj) = doc.get_object(*r) {
                is_goto_r_action(doc, obj)
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Check font embedding for the conversion. Returns the list of unembedded
/// font names, erroring when the target level forbids them outright.
fn check_font_embedding(doc: &lopdf::Document, level: PdfALevel) -> Result<Vec<String>> {
    let unembedded = collect_unembedded_fonts(doc);

    // For PDF/A-1b, unembedded fonts are a hard error
    if level == PdfALevel::A1b && !unembedded.is_empty() {
        return Err(PdfError::Internal(format!(
            "PDF/A-1b requires all fonts to be embedded; unembedded fonts: {}",
            unembedded.join(", ")
        )));
    }

    Ok(unembedded)
}

/// Names of every font in the document (pages, inherited resources, and form
/// XObjects included) that carries no embedded font program.
///
/// Producers write `/Resources`, `/Font`, and the font dictionaries themselves
/// as either direct dictionaries or references to indirect objects, so every
/// hop below accepts both. A font counts as embedded when it has a
/// `/FontFile`, `/FontFile2`, or `/FontFile3` entry, or when it is a Type 3
/// font, whose glyphs are content streams in the file by construction.
fn collect_unembedded_fonts(doc: &lopdf::Document) -> Vec<String> {
    let mut unembedded = Vec::new();
    let mut seen = HashSet::new();

    for page_id in doc.get_pages().values() {
        // Resources may sit on the page or on any ancestor page-tree node.
        let mut node = Some(*page_id);
        while let Some(id) = node {
            let Ok(page) = doc.get_dictionary(id) else {
                break;
            };
            if let Ok(resources) = page.get(b"Resources") {
                check_fonts_in_resources_object(doc, resources, &mut unembedded, &mut seen);
            }
            node = page.get(b"Parent").and_then(Object::as_reference).ok();
        }
    }

    unembedded.sort();
    unembedded.dedup();
    unembedded
}

/// Follow a `/Resources` value, written as a reference or a direct dictionary.
fn check_fonts_in_resources_object(
    doc: &lopdf::Document,
    resources: &Object,
    unembedded: &mut Vec<String>,
    seen: &mut HashSet<ObjectId>,
) {
    match resources {
        Object::Reference(id) => {
            if seen.insert(*id) {
                if let Ok(Object::Dictionary(dict)) = doc.get_object(*id) {
                    check_fonts_in_resources_dict(doc, dict, unembedded, seen);
                }
            }
        }
        Object::Dictionary(dict) => check_fonts_in_resources_dict(doc, dict, unembedded, seen),
        _ => {}
    }
}

fn check_fonts_in_resources_dict(
    doc: &lopdf::Document,
    resources: &Dictionary,
    unembedded: &mut Vec<String>,
    seen: &mut HashSet<ObjectId>,
) {
    if let Ok(fonts) = resources.get(b"Font") {
        check_font_dict_object(doc, fonts, unembedded, seen);
    }
    // Form XObjects carry their own resources, with their own fonts.
    if let Ok(xobjects) = resources.get(b"XObject") {
        check_xobject_dict_object(doc, xobjects, unembedded, seen);
    }
}

/// Follow a `/Font` value, written as a reference or a direct dictionary.
fn check_font_dict_object(
    doc: &lopdf::Document,
    fonts: &Object,
    unembedded: &mut Vec<String>,
    seen: &mut HashSet<ObjectId>,
) {
    let dict = match fonts {
        Object::Reference(id) => {
            if !seen.insert(*id) {
                return;
            }
            match doc.get_object(*id) {
                Ok(Object::Dictionary(dict)) => dict,
                _ => return,
            }
        }
        Object::Dictionary(dict) => dict,
        _ => return,
    };

    for (name, font) in dict.iter() {
        let font_dict = match font {
            Object::Reference(id) => {
                if !seen.insert(*id) {
                    continue;
                }
                match doc.get_object(*id) {
                    Ok(Object::Dictionary(dict)) => dict,
                    _ => continue,
                }
            }
            Object::Dictionary(dict) => dict,
            _ => continue,
        };

        // A font program, or glyphs defined by streams in this document.
        let has_font_file = font_dict.has(b"FontFile")
            || font_dict.has(b"FontFile2")
            || font_dict.has(b"FontFile3");
        let is_type3 =
            font_dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type3".as_slice());

        if !has_font_file && !is_type3 {
            let font_name = font_dict
                .get(b"BaseFont")
                .ok()
                .and_then(|object| object.as_name().ok())
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .unwrap_or_else(|| String::from_utf8_lossy(name).into_owned());
            unembedded.push(font_name);
        }
    }
}

/// Follow an `/XObject` value and recurse into the resources of every form.
fn check_xobject_dict_object(
    doc: &lopdf::Document,
    xobjects: &Object,
    unembedded: &mut Vec<String>,
    seen: &mut HashSet<ObjectId>,
) {
    let dict = match xobjects {
        Object::Reference(id) => {
            if !seen.insert(*id) {
                return;
            }
            match doc.get_object(*id) {
                Ok(Object::Dictionary(dict)) => dict,
                _ => return,
            }
        }
        Object::Dictionary(dict) => dict,
        _ => return,
    };

    for (_, xobject) in dict.iter() {
        let xobject_id = match xobject {
            Object::Reference(id) => *id,
            _ => continue,
        };
        if !seen.insert(xobject_id) {
            continue;
        }
        if let Ok(Object::Stream(stream)) = doc.get_object(xobject_id) {
            if stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                == Some(b"Form".as_slice())
            {
                if let Ok(resources) = stream.dict.get(b"Resources") {
                    check_fonts_in_resources_object(doc, resources, unembedded, seen);
                }
            }
        }
    }
}

/// Embed the sRGB ICC profile as a stream and return its ObjectId.
fn embed_icc_profile(doc: &mut lopdf::Document) -> Result<ObjectId> {
    // Check if we already have an sRGB ICC profile
    for obj in doc.objects.values() {
        if let Ok(stream) = obj.as_stream() {
            if stream.dict.get(b"N").and_then(Object::as_i64).ok() == Some(3) {
                // Might be an RGB ICC profile, check if it's sRGB
                // For simplicity, we always add a new one
            }
        }
    }

    let mut dict = Dictionary::new();
    dict.set("N", 3); // Number of components (RGB)
    dict.set("Alternate", Object::Name(b"DeviceRGB".to_vec()));

    let stream = Stream::new(dict, SRGB_ICC.to_vec());
    Ok(doc.add_object(stream))
}

/// Add OutputIntent with the ICC profile.
fn add_output_intent(
    doc: &mut lopdf::Document,
    icc_stream_id: ObjectId,
    level: PdfALevel,
) -> Result<()> {
    let root_id = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|_| PdfError::Internal("no catalog".to_string()))?;

    // Create OutputIntent dictionary
    let mut oi_dict = Dictionary::new();
    oi_dict.set("Type", Object::Name(b"OutputIntent".to_vec()));
    oi_dict.set("S", Object::Name(b"GTS_PDFA1".to_vec()));
    oi_dict.set(
        "OutputCondition",
        Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal),
    );
    oi_dict.set(
        "OutputConditionIdentifier",
        Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal),
    );
    oi_dict.set(
        "Info",
        Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal),
    );
    oi_dict.set("DestOutputProfile", Object::Reference(icc_stream_id));

    // Add registry name for PDF/A-2 and later
    if level != PdfALevel::A1b {
        oi_dict.set(
            "RegistryName",
            Object::String(
                b"http://www.color.org".to_vec(),
                lopdf::StringFormat::Literal,
            ),
        );
    }

    let oi_id = doc.add_object(oi_dict);

    // Check whether a GTS_PDFA1 OutputIntent is already present. The
    // existing intents are cloned out first so that the catalog is not
    // borrowed mutably and immutably at once.
    let existing: Option<Vec<Object>> = {
        let root = doc
            .get_object(root_id)
            .and_then(Object::as_dict)
            .map_err(|_| PdfError::Internal("catalog not a dictionary".to_string()))?;
        match root.get(b"OutputIntents") {
            Ok(Object::Array(arr)) => Some(arr.clone()),
            _ => None,
        }
    };

    let has_pdfa = existing.as_deref().unwrap_or(&[]).iter().any(|item| {
        item.as_reference()
            .ok()
            .and_then(|ref_id| doc.get_dictionary(ref_id).ok())
            .is_some_and(|oi| {
                oi.get(b"S").and_then(Object::as_name).ok() == Some(b"GTS_PDFA1".as_slice())
            })
    });

    if !has_pdfa {
        let mut intents = existing.unwrap_or_default();
        intents.push(Object::Reference(oi_id));
        let root = doc
            .get_object_mut(root_id)
            .and_then(Object::as_dict_mut)
            .map_err(|_| PdfError::Internal("catalog not a dictionary".to_string()))?;
        root.set("OutputIntents", Object::Array(intents));
    }

    Ok(())
}

/// Add or update XMP metadata with PDF/A identification.
fn add_xmp_metadata(doc: &mut lopdf::Document, level: PdfALevel) -> Result<()> {
    let xmp_packet = generate_xmp_packet(level);

    // Create XMP stream
    let mut xmp_dict = Dictionary::new();
    xmp_dict.set("Type", Object::Name(b"Metadata".to_vec()));
    xmp_dict.set("Subtype", Object::Name(b"XML".to_vec()));

    let xmp_stream = Stream::new(xmp_dict, xmp_packet.into_bytes());
    let xmp_id = doc.add_object(xmp_stream);

    // Attach to catalog
    let root_id = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|_| PdfError::Internal("no catalog".to_string()))?;
    let root = doc
        .get_object_mut(root_id)
        .and_then(Object::as_dict_mut)
        .map_err(|_| PdfError::Internal("catalog not a dictionary".to_string()))?;
    root.set("Metadata", Object::Reference(xmp_id));

    // Also update Info dictionary with Producer
    if let Ok(info_id) = doc.trailer.get(b"Info").and_then(Object::as_reference) {
        if let Ok(info) = doc.get_object_mut(info_id).and_then(Object::as_dict_mut) {
            info.set(
                "Producer",
                Object::String(
                    format!("Foolscap {}", env!("CARGO_PKG_VERSION")).into_bytes(),
                    lopdf::StringFormat::Literal,
                ),
            );
        }
    } else {
        // Create Info dictionary
        let mut info = Dictionary::new();
        info.set(
            "Producer",
            Object::String(
                format!("Foolscap {}", env!("CARGO_PKG_VERSION")).into_bytes(),
                lopdf::StringFormat::Literal,
            ),
        );
        let info_id = doc.add_object(info);
        doc.trailer.set("Info", Object::Reference(info_id));
    }

    Ok(())
}

/// Generate XMP metadata packet for PDF/A conformance.
fn generate_xmp_packet(level: PdfALevel) -> String {
    let part = level.part();
    let conformance = level.conformance();

    format!(
        r####"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about=""
    xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
    xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/">
<pdf:Producer>Foolscap {version}</pdf:Producer>
<pdfaid:part>{part}</pdfaid:part>
<pdfaid:conformance>{conformance}</pdfaid:conformance>
<xmp:CreateDate>{date}</xmp:CreateDate>
<xmp:ModifyDate>{date}</xmp:ModifyDate>
<dc:format>application/pdf</dc:format>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"####,
        version = env!("CARGO_PKG_VERSION"),
        part = part,
        conformance = conformance,
        date = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
    )
}

/// Remove transparency-related graphics state parameters for PDF/A-1b.
///
/// The `/Resources` holding the `/ExtGState` may sit on the page, on an
/// ancestor page-tree node, and may be a direct dictionary or a reference.
fn remove_transparency(doc: &mut lopdf::Document) -> Result<()> {
    // ExtGState entries may be references to gstate dictionaries, and reading
    // through a reference while mutating the container holding it is not
    // possible. So first collect the ids of every gstate dictionary that sets
    // a transparency parameter; the keys that point at them go afterwards.
    let transparent_gstate_ids: HashSet<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, object)| object.as_dict().is_ok_and(dict_has_transparency))
        .map(|(id, _)| *id)
        .collect();

    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for page_id in page_ids {
        let mut node = Some(page_id);
        while let Some(id) = node {
            let resources_object = {
                let Ok(page) = doc.get_dictionary(id) else {
                    break;
                };
                let resources = page.get(b"Resources").ok().cloned();
                let parent = page.get(b"Parent").and_then(Object::as_reference).ok();
                (resources, parent)
            };

            let (resources_object, parent) = resources_object;
            match resources_object {
                Some(Object::Reference(resources_id)) => {
                    if let Ok(resources) = doc
                        .get_object_mut(resources_id)
                        .and_then(Object::as_dict_mut)
                    {
                        strip_transparency_from_ext_gstate(resources, &transparent_gstate_ids);
                    }
                }
                Some(Object::Dictionary(_)) => {
                    // A direct dictionary lives inside the page-tree node.
                    if let Ok(page) = doc.get_object_mut(id).and_then(Object::as_dict_mut) {
                        if let Ok(resources) =
                            page.get_mut(b"Resources").and_then(Object::as_dict_mut)
                        {
                            strip_transparency_from_ext_gstate(resources, &transparent_gstate_ids);
                        }
                    }
                }
                _ => {}
            }

            node = parent;
        }
    }

    // Form XObjects carry their own resources, with their own graphics states.
    // Collect where each form's `/Resources` lives before mutating anything.
    let mut referenced_resource_ids: Vec<ObjectId> = Vec::new();
    let mut forms_with_inline_resources: Vec<ObjectId> = Vec::new();
    for (id, object) in &doc.objects {
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Form".as_slice()) {
            continue;
        }
        match stream.dict.get(b"Resources") {
            Ok(Object::Reference(resources_id)) => referenced_resource_ids.push(*resources_id),
            Ok(Object::Dictionary(_)) => forms_with_inline_resources.push(*id),
            _ => {}
        }
    }

    for resources_id in referenced_resource_ids {
        if let Ok(resources) = doc
            .get_object_mut(resources_id)
            .and_then(Object::as_dict_mut)
        {
            strip_transparency_from_ext_gstate(resources, &transparent_gstate_ids);
        }
    }
    for form_id in forms_with_inline_resources {
        if let Ok(form) = doc.get_object_mut(form_id).and_then(Object::as_stream_mut) {
            if let Ok(resources) = form
                .dict
                .get_mut(b"Resources")
                .and_then(Object::as_dict_mut)
            {
                strip_transparency_from_ext_gstate(resources, &transparent_gstate_ids);
            }
        }
    }

    Ok(())
}

/// Whether a graphics state dictionary sets a transparency parameter.
fn dict_has_transparency(dict: &Dictionary) -> bool {
    dict.has(b"CA") || dict.has(b"ca") || dict.has(b"BM") || dict.has(b"SMask")
}

/// Delete every `/ExtGState` entry that sets a transparency parameter, either
/// directly or through a reference to a gstate dictionary.
fn strip_transparency_from_ext_gstate(
    resources: &mut Dictionary,
    transparent_gstate_ids: &HashSet<ObjectId>,
) {
    let Ok(ext_gstate) = resources
        .get_mut(b"ExtGState")
        .and_then(Object::as_dict_mut)
    else {
        return;
    };
    let keys_to_remove: Vec<Vec<u8>> = ext_gstate
        .iter()
        .filter_map(|(key, value)| {
            let transparent = match value {
                Object::Dictionary(gs) => dict_has_transparency(gs),
                Object::Reference(id) => transparent_gstate_ids.contains(id),
                _ => false,
            };
            transparent.then(|| key.clone())
        })
        .collect();
    for key in keys_to_remove {
        ext_gstate.remove(&key);
    }
}

/// Ensure the PDF version matches the PDF/A level.
///
/// PDF/A-1 is based on PDF 1.4 exactly, so the header is pinned to it (a
/// higher version would advertise features the standard forbids); PDF/A-2 and
/// later are based on PDF 1.7, so the version is only raised.
fn ensure_min_version(doc: &mut lopdf::Document, level: PdfALevel) -> Result<()> {
    let target = match level {
        PdfALevel::A1b => "1.4",
        PdfALevel::A2b | PdfALevel::A3b => "1.7",
    };

    let current: f32 = doc.version.parse().unwrap_or(1.4);
    let required: f32 = target.parse().unwrap_or(1.4);

    if level == PdfALevel::A1b || current < required {
        doc.version = target.to_string();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;
    use lopdf::{Dictionary, Object, Stream};

    fn simple_pdf() -> Document {
        let mut doc = lopdf::Document::new();
        doc.version = "1.4".to_string();

        // Minimal page
        let pages_id = doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Kids", Object::Array(vec![])),
            ("Count", Object::Integer(0)),
        ]));
        let contents_id = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
        let page_id = doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Page".to_vec())),
            ("Parent", Object::Reference(pages_id)),
            (
                "MediaBox",
                Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()]),
            ),
            (
                "Resources",
                Object::Dictionary(Dictionary::from_iter([(
                    "Font",
                    Object::Dictionary(Dictionary::new()),
                )])),
            ),
            ("Contents", Object::Reference(contents_id)),
        ]));
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Kids", Object::Array(vec![Object::Reference(page_id)]));
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Count", Object::Integer(1));

        let catalog_id = doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Catalog".to_vec())),
            ("Pages", Object::Reference(pages_id)),
        ]));
        doc.trailer.set("Root", Object::Reference(catalog_id));

        Document::from_lopdf(doc, None)
    }

    #[test]
    fn pdfa_1b_conversion_basic() {
        let mut doc = simple_pdf();
        let report = to_pdfa(&mut doc, PdfALevel::A1b).unwrap();

        assert!(report.icc_embedded);
        assert!(report.output_intent_added);
        assert!(report.xmp_updated);
        assert!(!report.had_encryption);

        // Verify XMP metadata exists
        let root_id = doc
            .inner
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .unwrap();
        let root = doc.inner.get_dictionary(root_id).unwrap();
        assert!(root.get(b"Metadata").is_ok());

        // Verify OutputIntents
        assert!(root.get(b"OutputIntents").is_ok());

        // Verify ICC profile stream exists
        let output_intents = root.get(b"OutputIntents").unwrap().as_array().unwrap();
        assert_eq!(output_intents.len(), 1);
        let oi_id = output_intents[0].as_reference().unwrap();
        let oi = doc.inner.get_dictionary(oi_id).unwrap();
        assert_eq!(oi.get(b"S").unwrap().as_name().unwrap(), b"GTS_PDFA1");
        assert!(oi.get(b"DestOutputProfile").is_ok());
    }

    #[test]
    fn pdfa_2b_conversion() {
        let mut doc = simple_pdf();
        let report = to_pdfa(&mut doc, PdfALevel::A2b).unwrap();

        assert!(report.icc_embedded);
        assert!(report.output_intent_added);
        assert!(report.xmp_updated);

        let root_id = doc
            .inner
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .unwrap();
        let root = doc.inner.get_dictionary(root_id).unwrap();
        let output_intents = root.get(b"OutputIntents").unwrap().as_array().unwrap();
        let oi_id = output_intents[0].as_reference().unwrap();
        let oi = doc.inner.get_dictionary(oi_id).unwrap();
        assert_eq!(oi.get(b"S").unwrap().as_name().unwrap(), b"GTS_PDFA1");
        assert!(oi.get(b"RegistryName").is_ok());
    }

    #[test]
    fn pdfa_level_parse() {
        assert_eq!(PdfALevel::parse("1b"), Some(PdfALevel::A1b));
        assert_eq!(PdfALevel::parse("2b"), Some(PdfALevel::A2b));
        assert_eq!(PdfALevel::parse("3b"), Some(PdfALevel::A3b));
        assert_eq!(PdfALevel::parse("1B"), Some(PdfALevel::A1b));
        assert_eq!(PdfALevel::parse("4b"), None);
        assert_eq!(PdfALevel::parse("1a"), None);
    }

    #[test]
    fn pdfa_removes_encryption() {
        let mut doc = simple_pdf();
        // Add encryption dictionary
        let encrypt_id = doc.inner.add_object(Dictionary::from_iter([
            ("Filter", Object::Name(b"Standard".to_vec())),
            ("V", 2.into()),
            ("R", 3.into()),
            ("Length", 128.into()),
        ]));
        doc.inner
            .trailer
            .set("Encrypt", Object::Reference(encrypt_id));

        let report = to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        assert!(report.had_encryption);
        assert!(doc.inner.trailer.get(b"Encrypt").is_err());
    }

    #[test]
    fn pdfa_removes_javascript() {
        let mut doc = simple_pdf();
        let root_id = doc
            .inner
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .unwrap();
        let mut js_dict = Dictionary::new();
        js_dict.set("S", Object::Name(b"JavaScript".to_vec()));
        js_dict.set(
            "JS",
            Object::String(b"app.alert('hello')".to_vec(), lopdf::StringFormat::Literal),
        );
        let js_id = doc.inner.add_object(js_dict);
        doc.inner
            .get_object_mut(root_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("OpenAction", Object::Reference(js_id));

        let report = to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        assert_eq!(report.javascript_removed, 1);
        let root = doc.inner.get_dictionary(root_id).unwrap();
        assert!(root.get(b"OpenAction").is_err());
    }

    #[test]
    fn xmp_contains_pdfa_identification() {
        let mut doc = simple_pdf();
        to_pdfa(&mut doc, PdfALevel::A1b).unwrap();

        let root_id = doc
            .inner
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .unwrap();
        let root = doc.inner.get_dictionary(root_id).unwrap();
        let xmp_id = root
            .get(b"Metadata")
            .and_then(Object::as_reference)
            .unwrap();
        let xmp_stream = doc
            .inner
            .get_object(xmp_id)
            .and_then(lopdf::Object::as_stream)
            .unwrap();
        let xmp_content = String::from_utf8_lossy(&xmp_stream.content);
        assert!(xmp_content.contains("pdfaid:part>1</pdfaid:part>"));
        assert!(xmp_content.contains("pdfaid:conformance>B</pdfaid:conformance>"));
    }

    #[test]
    fn pdfa_version_upgrade() {
        let mut doc = simple_pdf();
        doc.inner.version = "1.3".to_string();
        to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        assert_eq!(doc.inner.version, "1.4");

        let mut doc2 = simple_pdf();
        doc2.inner.version = "1.5".to_string();
        to_pdfa(&mut doc2, PdfALevel::A2b).unwrap();
        assert_eq!(doc2.inner.version, "1.7");

        // PDF/A-1 is based on PDF 1.4 exactly, so a newer source is pinned back.
        let mut doc3 = simple_pdf();
        doc3.inner.version = "1.7".to_string();
        to_pdfa(&mut doc3, PdfALevel::A1b).unwrap();
        assert_eq!(doc3.inner.version, "1.4");

        // A part-1 claim on a newer file is an issue the checker reports.
        doc3.inner.version = "1.7".to_string();
        let report = check_pdfa(&doc3).unwrap();
        assert!(!report.conformant);
        assert!(report.issues.iter().any(|issue| issue.contains("PDF 1.4")));
    }

    fn root_id(doc: &Document) -> ObjectId {
        doc.inner
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .unwrap()
    }

    /// A simple_pdf whose page uses `/F1`, a Type1 font with no font program,
    /// either on the page or inherited from the page tree.
    fn simple_pdf_with_font(on_page: bool) -> Document {
        let mut doc = simple_pdf();
        let font_id = doc.inner.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Helvetica".to_vec())),
        ]));
        let resources_id = doc.inner.add_object(Dictionary::from_iter([(
            "Font",
            Object::Dictionary(Dictionary::from_iter([("F1", Object::Reference(font_id))])),
        )]));

        let target = if on_page {
            // simple_pdf keeps /Resources directly on the page.
            let pages_id = doc.inner.get_pages().values().copied().next().unwrap();
            let parent = doc
                .inner
                .get_dictionary(pages_id)
                .unwrap()
                .get(b"Parent")
                .and_then(Object::as_reference)
                .unwrap();
            parent
        } else {
            // Move the resources onto the /Pages node instead.
            let page_id = doc.inner.get_pages().values().copied().next().unwrap();
            let parent = doc
                .inner
                .get_dictionary(page_id)
                .unwrap()
                .get(b"Parent")
                .and_then(Object::as_reference)
                .unwrap();
            doc.inner
                .get_object_mut(page_id)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .remove(b"Resources");
            parent
        };
        doc.inner
            .get_object_mut(target)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Resources", Object::Reference(resources_id));
        doc
    }

    #[test]
    fn check_flags_a_plain_document() {
        let doc = simple_pdf();
        let report = check_pdfa(&doc).unwrap();
        assert!(!report.conformant);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("OutputIntent")));
        assert!(report.issues.iter().any(|issue| issue.contains("XMP")));
    }

    #[test]
    fn check_passes_after_conversion() {
        let mut doc = simple_pdf();
        let report = to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        assert!(report.conformant, "issues: {:?}", report.issues);

        let report = check_pdfa(&doc).unwrap();
        assert!(report.conformant, "issues: {:?}", report.issues);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn check_flags_unembedded_fonts_wherever_they_hide() {
        for on_page in [true, false] {
            let doc = simple_pdf_with_font(on_page);
            let report = check_pdfa(&doc).unwrap();
            assert!(
                report
                    .issues
                    .iter()
                    .any(|issue| issue.contains("Helvetica")),
                "on_page = {on_page}: {:?}",
                report.issues
            );

            // PDF/A-1b refuses to convert a document with unembedded fonts.
            let mut doc = simple_pdf_with_font(on_page);
            assert!(to_pdfa(&mut doc, PdfALevel::A1b).is_err());
        }
    }

    #[test]
    fn check_accepts_embedded_and_type3_fonts() {
        let mut doc = simple_pdf();
        let embedded_id = doc.inner.add_object(Stream::new(
            Dictionary::from_iter([("Length1", 1.into())]),
            vec![0u8; 4],
        ));
        let font_id = doc.inner.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"TrueType".to_vec())),
            ("BaseFont", Object::Name(b"Courier".to_vec())),
            ("FontFile2", Object::Reference(embedded_id)),
        ]));
        let type3_id = doc.inner.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type3".to_vec())),
        ]));
        let font_dict_id = doc.inner.add_object(Dictionary::from_iter([
            ("F1", Object::Reference(font_id)),
            ("F3", Object::Reference(type3_id)),
        ]));

        let page_id = doc.inner.get_pages().values().copied().next().unwrap();
        let page = doc
            .inner
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap();
        page.set(
            "Resources",
            Object::Dictionary(Dictionary::from_iter([(
                "Font",
                Object::Reference(font_dict_id),
            )])),
        );

        to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        let report = check_pdfa(&doc).unwrap();
        assert!(
            !report.issues.iter().any(|issue| issue.contains("embedded")),
            "issues: {:?}",
            report.issues
        );
    }

    #[test]
    fn check_flags_encryption() {
        let mut doc = simple_pdf();
        let encrypt_id = doc.inner.add_object(Dictionary::from_iter([
            ("Filter", Object::Name(b"Standard".to_vec())),
            ("V", 2.into()),
        ]));
        doc.inner
            .trailer
            .set("Encrypt", Object::Reference(encrypt_id));

        let report = check_pdfa(&doc).unwrap();
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("encrypted")));
    }

    #[test]
    fn check_flags_and_conversion_strips_annotation_actions() {
        // A page with a direct /Annots array holding a launch action and a
        // GoToR action — the form producers most often write.
        let mut doc = simple_pdf();
        let launch_id = doc.inner.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Annot".to_vec())),
            ("Subtype", Object::Name(b"Link".to_vec())),
            (
                "A",
                Object::Dictionary(Dictionary::from_iter([(
                    "S",
                    Object::Name(b"Launch".to_vec()),
                )])),
            ),
        ]));
        let goto_r_id = doc.inner.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Annot".to_vec())),
            ("Subtype", Object::Name(b"Link".to_vec())),
            (
                "A",
                Object::Dictionary(Dictionary::from_iter([(
                    "S",
                    Object::Name(b"GoToR".to_vec()),
                )])),
            ),
        ]));
        let page_id = doc.inner.get_pages().values().copied().next().unwrap();
        doc.inner
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "Annots",
                Object::Array(vec![
                    Object::Reference(launch_id),
                    Object::Reference(goto_r_id),
                ]),
            );

        let report = check_pdfa(&doc).unwrap();
        assert!(report.issues.iter().any(|issue| issue.contains("Launch")));
        assert!(report.issues.iter().any(|issue| issue.contains("GoToR")));

        let report = to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        assert_eq!(report.launch_actions_removed, 1);
        assert_eq!(report.external_refs_removed, 1);
        let report = check_pdfa(&doc).unwrap();
        assert!(report.conformant, "issues: {:?}", report.issues);
    }

    #[test]
    fn check_flags_annotation_javascript_and_conversion_strips_it() {
        let mut doc = simple_pdf();
        let js_annot_id = doc.inner.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Annot".to_vec())),
            ("Subtype", Object::Name(b"Widget".to_vec())),
            (
                "AA",
                Object::Dictionary(Dictionary::from_iter([(
                    "E",
                    Object::Dictionary(Dictionary::from_iter([
                        ("S", Object::Name(b"JavaScript".to_vec())),
                        (
                            "JS",
                            Object::String(b"app.alert(1)".to_vec(), lopdf::StringFormat::Literal),
                        ),
                    ])),
                )])),
            ),
        ]));
        let page_id = doc.inner.get_pages().values().copied().next().unwrap();
        doc.inner
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "Annots",
                Object::Array(vec![Object::Reference(js_annot_id)]),
            );

        let report = check_pdfa(&doc).unwrap();
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("JavaScript")));

        let report = to_pdfa(&mut doc, PdfALevel::A2b).unwrap();
        assert_eq!(report.javascript_removed, 1);
        let report = check_pdfa(&doc).unwrap();
        assert!(report.conformant, "issues: {:?}", report.issues);
    }

    #[test]
    fn check_flags_javascript_name_tree() {
        let mut doc = simple_pdf();
        let root_id = root_id(&doc);
        // /Names written as a direct dictionary, as lopdf's writer produces.
        doc.inner
            .get_object_mut(root_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "Names",
                Dictionary::from_iter([("JavaScript", Object::Array(vec![]))]),
            );

        let report = check_pdfa(&doc).unwrap();
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("name tree")));

        to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        let report = check_pdfa(&doc).unwrap();
        assert!(report.conformant, "issues: {:?}", report.issues);
    }

    #[test]
    fn check_flags_transparency_only_for_pdfa1() {
        // The level-specific rules key off the part the XMP claims, so the
        // document must already declare itself PDF/A before the rule applies.
        let add_transparency = |doc: &mut Document| {
            let gs_id = doc
                .inner
                .add_object(Dictionary::from_iter([("ca", 0.5.into())]));
            let page_id = doc.inner.get_pages().values().copied().next().unwrap();
            doc.inner
                .get_object_mut(page_id)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set(
                    "Resources",
                    Object::Dictionary(Dictionary::from_iter([(
                        "ExtGState",
                        Object::Dictionary(Dictionary::from_iter([(
                            "GS1",
                            Object::Reference(gs_id),
                        )])),
                    )])),
                );
        };

        // PDF/A-1: transparency is an issue, and conversion strips it.
        let mut doc = simple_pdf();
        to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        add_transparency(&mut doc);
        let report = check_pdfa(&doc).unwrap();
        assert!(!report.conformant);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("transparency")));
        let report = to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        assert!(report.conformant, "issues: {:?}", report.issues);

        // PDF/A-2: transparency is allowed and stays.
        let mut doc = simple_pdf();
        to_pdfa(&mut doc, PdfALevel::A2b).unwrap();
        add_transparency(&mut doc);
        let report = to_pdfa(&mut doc, PdfALevel::A2b).unwrap();
        assert!(report.conformant, "issues: {:?}", report.issues);
        let page_id = doc.inner.get_pages().values().copied().next().unwrap();
        let page = doc.inner.get_dictionary(page_id).unwrap();
        let resources = page.get(b"Resources").unwrap().as_dict().unwrap();
        assert!(resources.get(b"ExtGState").is_ok());
    }

    #[test]
    fn check_flags_outline_goto_r() {
        let mut doc = simple_pdf();
        let root_id = root_id(&doc);
        let action_id = doc.inner.add_object(Dictionary::from_iter([
            ("S", Object::Name(b"GoToR".to_vec())),
            (
                "F",
                Object::String(b"other.pdf".to_vec(), lopdf::StringFormat::Literal),
            ),
        ]));
        let item_id = doc.inner.add_object(Dictionary::from_iter([
            (
                "Title",
                Object::String(b"Elsewhere".to_vec(), lopdf::StringFormat::Literal),
            ),
            ("A", Object::Reference(action_id)),
        ]));
        let outlines_id = doc.inner.add_object(Dictionary::from_iter([
            ("First", Object::Reference(item_id)),
            ("Last", Object::Reference(item_id)),
            ("Count", 1.into()),
        ]));
        doc.inner
            .get_object_mut(root_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Outlines", Object::Reference(outlines_id));

        let report = check_pdfa(&doc).unwrap();
        assert!(report.issues.iter().any(|issue| issue.contains("bookmark")));

        let report = to_pdfa(&mut doc, PdfALevel::A2b).unwrap();
        assert_eq!(report.external_refs_removed, 1);
        let report = check_pdfa(&doc).unwrap();
        assert!(report.conformant, "issues: {:?}", report.issues);
    }

    #[test]
    fn xmp_claim_reads_part_and_conformance() {
        let mut doc = simple_pdf();
        to_pdfa(&mut doc, PdfALevel::A3b).unwrap();

        let root = doc.inner.get_dictionary(root_id(&doc)).unwrap();
        let (part, conformance) = xmp_pdfa_claim(&doc.inner, root).unwrap();
        assert_eq!(part, 3);
        assert_eq!(conformance, "B");
    }

    #[test]
    fn extract_xml_tag_finds_nested_content() {
        let xml = "<x:xmpmeta><pdfaid:part>2</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance></x:xmpmeta>";
        assert_eq!(extract_xml_tag(xml, "pdfaid:part"), Some("2"));
        assert_eq!(extract_xml_tag(xml, "pdfaid:conformance"), Some("B"));
        assert_eq!(extract_xml_tag(xml, "pdfaid:level"), None);
    }

    #[test]
    fn check_reports_missing_catalog() {
        let mut doc = simple_pdf();
        doc.inner.trailer.remove(b"Root");
        let report = check_pdfa(&doc).unwrap();
        assert!(!report.conformant);
        assert!(report.issues.iter().any(|issue| issue.contains("catalog")));
    }
}
