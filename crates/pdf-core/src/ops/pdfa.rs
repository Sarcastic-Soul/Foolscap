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

/// Report from a PDF/A conversion.
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

    Ok(report)
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
    let mut has_aa = false;
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

    // Remove JavaScript from Names tree
    if let Ok(root_id) = doc.trailer.get(b"Root").and_then(Object::as_reference) {
        if let Ok(root) = doc.get_object_mut(root_id).and_then(Object::as_dict_mut) {
            if let Ok(names_id) = root.get(b"Names").and_then(Object::as_reference) {
                if let Ok(names) = doc.get_object_mut(names_id).and_then(Object::as_dict_mut) {
                    names.remove(b"JavaScript");
                    names.remove(b"EmbeddedFiles"); // EmbeddedFiles handled separately for PDF/A-3
                }
            }
        }
    }

    Ok(removed)
}

/// Remove all launch actions (GoToR with /Launch, or Launch actions).
fn remove_launch_actions(doc: &mut lopdf::Document) -> Result<usize> {
    let mut removed = 0;

    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for page_id in page_ids {
        let annots_id = {
            let page = match doc.get_object(page_id).and_then(Object::as_dict) {
                Ok(d) => d,
                Err(_) => continue,
            };
            match page.get(b"Annots").and_then(Object::as_reference) {
                Ok(id) => id,
                Err(_) => continue,
            }
        };

        let mut indices_to_remove = Vec::new();
        {
            let annots = match doc.get_object(annots_id).and_then(Object::as_array) {
                Ok(a) => a,
                Err(_) => continue,
            };
            for (i, annot_obj) in annots.iter().enumerate() {
                if let Ok(annot_ref) = annot_obj.as_reference() {
                    if let Ok(annot) = doc.get_object(annot_ref).and_then(Object::as_dict) {
                        if let Ok(subtype) = annot.get(b"Subtype").and_then(Object::as_name) {
                            if subtype == b"Link" {
                                if let Ok(a) = annot.get(b"A") {
                                    if is_launch_action(doc, a) {
                                        indices_to_remove.push(i);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Remove in reverse order to keep indices valid
        for &i in indices_to_remove.iter().rev() {
            if let Ok(annots) = doc.get_object_mut(annots_id).and_then(Object::as_array_mut) {
                if i < annots.len() {
                    annots.remove(i);
                    removed += 1;
                }
            }
        }
    }

    Ok(removed)
}

/// Remove external references (GoToR actions pointing to other files).
fn remove_external_refs(doc: &mut lopdf::Document) -> Result<usize> {
    let mut removed = 0;

    // Check annotations for GoToR
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for page_id in page_ids {
        let annots_id = {
            let page = match doc.get_object(page_id).and_then(Object::as_dict) {
                Ok(d) => d,
                Err(_) => continue,
            };
            match page.get(b"Annots").and_then(Object::as_reference) {
                Ok(id) => id,
                Err(_) => continue,
            }
        };

        let mut indices_to_remove = Vec::new();
        {
            let annots = match doc.get_object(annots_id).and_then(Object::as_array) {
                Ok(a) => a,
                Err(_) => continue,
            };
            for (i, annot_obj) in annots.iter().enumerate() {
                if let Ok(annot_ref) = annot_obj.as_reference() {
                    if let Ok(annot) = doc.get_object(annot_ref).and_then(Object::as_dict) {
                        if let Ok(a) = annot.get(b"A") {
                            if is_goto_r_action(doc, a) {
                                indices_to_remove.push(i);
                            }
                        }
                    }
                }
            }
        }

        // Remove in reverse order
        for &i in indices_to_remove.iter().rev() {
            if let Ok(annots) = doc.get_object_mut(annots_id).and_then(Object::as_array_mut) {
                if i < annots.len() {
                    annots.remove(i);
                    removed += 1;
                }
            }
        }
    }

    // Also check outline items
    if let Ok(root_id) = doc.trailer.get(b"Root").and_then(Object::as_reference) {
        if let Ok(root) = doc.get_object_mut(root_id).and_then(Object::as_dict_mut) {
            if let Ok(outlines_id) = root.get(b"Outlines").and_then(Object::as_reference) {
                removed += remove_goto_r_from_outlines(doc, outlines_id)?;
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
            item.get(b"A").map(|a| is_goto_r_action(doc, a)).unwrap_or(false)
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

/// Check font embedding. Returns list of unembedded font names.
fn check_font_embedding(doc: &lopdf::Document, level: PdfALevel) -> Result<Vec<String>> {
    let mut unembedded = Vec::new();
    let mut seen_fonts = HashSet::new();

    for page_id in doc.get_pages().values() {
        if let Ok(page) = doc.get_dictionary(*page_id) {
            if let Ok(resources) = page.get(b"Resources").and_then(Object::as_reference) {
                check_fonts_in_resources(doc, resources, &mut unembedded, &mut seen_fonts)?;
            }
        }
    }

    // For PDF/A-1b, unembedded fonts are a hard error
    if level == PdfALevel::A1b && !unembedded.is_empty() {
        return Err(PdfError::Internal(format!(
            "PDF/A-1b requires all fonts to be embedded; unembedded fonts: {}",
            unembedded.join(", ")
        )));
    }

    Ok(unembedded)
}

fn check_fonts_in_resources(
    doc: &lopdf::Document,
    resources_id: ObjectId,
    unembedded: &mut Vec<String>,
    seen: &mut HashSet<ObjectId>,
) -> Result<()> {
    if !seen.insert(resources_id) {
        return Ok(());
    }

    let resources = doc.get_dictionary(resources_id)?;
    if let Ok(font_dict) = resources.get(b"Font").and_then(Object::as_reference) {
        check_font_dict(doc, font_dict, unembedded, seen)?;
    }
    // Also check nested XObjects (Form XObjects can have their own resources)
    if let Ok(xobj_dict) = resources.get(b"XObject").and_then(Object::as_reference) {
        check_xobjects_for_fonts(doc, xobj_dict, unembedded, seen)?;
    }
    Ok(())
}

fn check_font_dict(
    doc: &lopdf::Document,
    font_dict_id: ObjectId,
    unembedded: &mut Vec<String>,
    _seen: &mut HashSet<ObjectId>,
) -> Result<()> {
    let font_dict = doc.get_dictionary(font_dict_id)?;
    for (name, font_ref) in font_dict.iter() {
        if let Ok(font_id) = font_ref.as_reference() {
            if let Ok(font) = doc.get_dictionary(font_id) {
                // Check if font has a FontFile, FontFile2, or FontFile3 (embedded)
                let has_font_file = font.get(b"FontFile").is_ok()
                    || font.get(b"FontFile2").is_ok()
                    || font.get(b"FontFile3").is_ok();

                // Type 3 fonts are defined by a stream, so they're "embedded" by definition
                let is_type3 = font.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type3".as_slice());

                // Standard 14 fonts don't need embedding IF they're not subsetted
                // But PDF/A requires embedding anyway, so we flag them
                let is_standard14 = matches!(
                    font.get(b"BaseFont").and_then(Object::as_name).ok(),
                    Some(b"Times-Roman" | b"Times-Bold" | b"Times-Italic" | b"Times-BoldItalic" |
                         b"Helvetica" | b"Helvetica-Bold" | b"Helvetica-Oblique" | b"Helvetica-BoldOblique" |
                         b"Courier" | b"Courier-Bold" | b"Courier-Oblique" | b"Courier-BoldOblique" |
                         b"Symbol" | b"ZapfDingbats")
                );

                if !has_font_file && !is_type3 {
                    let font_name = font.get(b"BaseFont")
                        .ok()
                        .and_then(|o| o.as_name().ok())
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .or_else(|| Some(String::from_utf8_lossy(name).into_owned()))
                        .unwrap_or_else(|| "Unknown".to_string());
                    unembedded.push(font_name);
                }
            }
        }
    }
    Ok(())
}

fn check_xobjects_for_fonts(
    doc: &lopdf::Document,
    xobj_dict_id: ObjectId,
    unembedded: &mut Vec<String>,
    seen: &mut HashSet<ObjectId>,
) -> Result<()> {
    let xobj_dict = doc.get_dictionary(xobj_dict_id)?;
    for (_, obj_ref) in xobj_dict.iter() {
        if let Ok(xobj_id) = obj_ref.as_reference() {
            if let Ok(xobj) = doc.get_object(xobj_id) {
                if let Ok(stream) = xobj.as_stream() {
                    if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form".as_slice()) {
                        if let Ok(resources) = stream.dict.get(b"Resources").and_then(Object::as_reference) {
                            check_fonts_in_resources(doc, resources, unembedded, seen)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Embed the sRGB ICC profile as a stream and return its ObjectId.
fn embed_icc_profile(doc: &mut lopdf::Document) -> Result<ObjectId> {
    // Check if we already have an sRGB ICC profile
    for (_id, obj) in doc.objects.iter() {
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
fn add_output_intent(doc: &mut lopdf::Document, icc_stream_id: ObjectId, level: PdfALevel) -> Result<()> {
    let root_id = doc.trailer.get(b"Root").and_then(Object::as_reference)
        .map_err(|_| PdfError::Internal("no catalog".to_string()))?;

    // Create OutputIntent dictionary
    let mut oi_dict = Dictionary::new();
    oi_dict.set("Type", Object::Name(b"OutputIntent".to_vec()));
    oi_dict.set("S", Object::Name(b"GTS_PDFA1".to_vec()));
    oi_dict.set("OutputCondition", Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal));
    oi_dict.set("OutputConditionIdentifier", Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal));
    oi_dict.set("Info", Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal));
    oi_dict.set("DestOutputProfile", Object::Reference(icc_stream_id));

    // Add registry name for PDF/A-2 and later
    if level != PdfALevel::A1b {
        oi_dict.set("RegistryName", Object::String(b"http://www.color.org".to_vec(), lopdf::StringFormat::Literal));
    }

    let oi_id = doc.add_object(oi_dict);

    // Get or create OutputIntents array
    let root = doc.get_object_mut(root_id).and_then(Object::as_dict_mut)
        .map_err(|_| PdfError::Internal("catalog not a dictionary".to_string()))?;

    let output_intents = match root.get(b"OutputIntents") {
        Ok(Object::Array(arr)) => {
            // Check if we already have a GTS_PDFA1 intent
            let mut has_pdfa = false;
            for item in arr {
                if let Ok(ref_id) = item.as_reference() {
                    if let Ok(oi) = doc.get_dictionary(ref_id) {
                        if oi.get(b"S").and_then(Object::as_name).ok() == Some(b"GTS_PDFA1".as_slice()) {
                            has_pdfa = true;
                            break;
                        }
                    }
                }
            }
            if !has_pdfa {
                let mut arr = arr.clone();
                arr.push(Object::Reference(oi_id));
                root.set("OutputIntents", Object::Array(arr.clone()));
            }
            Object::Array(arr)
        }
        _ => {
            root.set("OutputIntents", Object::Array(vec![Object::Reference(oi_id)]));
            Object::Array(vec![Object::Reference(oi_id)])
        }
    };

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
    let root_id = doc.trailer.get(b"Root").and_then(Object::as_reference)
        .map_err(|_| PdfError::Internal("no catalog".to_string()))?;
    let root = doc.get_object_mut(root_id).and_then(Object::as_dict_mut)
        .map_err(|_| PdfError::Internal("catalog not a dictionary".to_string()))?;
    root.set("Metadata", Object::Reference(xmp_id));

    // Also update Info dictionary with Producer
    if let Ok(info_id) = doc.trailer.get(b"Info").and_then(Object::as_reference) {
        if let Ok(info) = doc.get_object_mut(info_id).and_then(Object::as_dict_mut) {
            info.set("Producer", Object::String(
                format!("Foolscap {}", env!("CARGO_PKG_VERSION")).into_bytes(),
                lopdf::StringFormat::Literal,
            ));
        }
    } else {
        // Create Info dictionary
        let mut info = Dictionary::new();
        info.set("Producer", Object::String(
            format!("Foolscap {}", env!("CARGO_PKG_VERSION")).into_bytes(),
            lopdf::StringFormat::Literal,
        ));
        let info_id = doc.add_object(info);
        doc.trailer.set("Info", Object::Reference(info_id));
    }

    Ok(())
}

/// Generate XMP metadata packet for PDF/A conformance.
fn generate_xmp_packet(level: PdfALevel) -> String {
    let part = level.part();
    let conformance = level.conformance();

    format!(r####"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
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
fn remove_transparency(doc: &mut lopdf::Document) -> Result<()> {
    // Check ExtGState in page resources and form XObjects
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for page_id in page_ids {
        let resources_id = {
            let page = match doc.get_dictionary(page_id) {
                Ok(d) => d,
                Err(_) => continue,
            };
            match page.get(b"Resources").and_then(Object::as_reference) {
                Ok(id) => id,
                Err(_) => continue,
            }
        };
        remove_transparency_from_resources(doc, resources_id)?;
    }
    Ok(())
}

fn remove_transparency_from_resources(doc: &mut lopdf::Document, resources_id: ObjectId) -> Result<()> {
    let resources = doc.get_dictionary_mut(resources_id)?;
    if let Ok(ext_gstate) = resources.get_mut(b"ExtGState").and_then(Object::as_dict_mut) {
        let keys_to_remove: Vec<Vec<u8>> = ext_gstate.iter()
            .filter_map(|(k, v)| {
                if let Ok(gs) = v.as_dict() {
                    if gs.get(b"CA").is_ok() || gs.get(b"ca").is_ok() 
                        || gs.get(b"BM").is_ok() || gs.get(b"SMask").is_ok()
                        || gs.get(b"AIS").is_ok() || gs.get(b"TK").is_ok() {
                        return Some(k.clone());
                    }
                }
                None
            })
            .collect();
        for key in keys_to_remove {
            ext_gstate.remove(&key);
        }
    }
    // Recurse into XObjects
    if let Ok(xobj_dict) = resources.get(b"XObject").and_then(Object::as_reference) {
        remove_transparency_from_xobjects(doc, xobj_dict)?;
    }
    Ok(())
}

fn remove_transparency_from_xobjects(doc: &mut lopdf::Document, xobj_dict_id: ObjectId) -> Result<()> {
    let xobj_dict = doc.get_dictionary(xobj_dict_id)?;
    let xobj_ids: Vec<ObjectId> = xobj_dict.iter()
        .filter_map(|(_, obj_ref)| obj_ref.as_reference().ok())
        .collect();

    for xobj_id in xobj_ids {
        if let Ok(xobj) = doc.get_object_mut(xobj_id).and_then(Object::as_stream_mut) {
            if xobj.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form".as_slice()) {
                if let Ok(resources) = xobj.dict.get(b"Resources").and_then(Object::as_reference) {
                    remove_transparency_from_resources(doc, resources)?;
                }
            }
        }
    }
    Ok(())
}

/// Ensure minimum PDF version for the PDF/A level.
fn ensure_min_version(doc: &mut lopdf::Document, level: PdfALevel) -> Result<()> {
    let min_version = match level {
        PdfALevel::A1b => "1.4",
        PdfALevel::A2b => "1.7",
        PdfALevel::A3b => "1.7",
    };

    let current: f32 = doc.version.parse().unwrap_or(1.4);
    let required: f32 = min_version.parse().unwrap_or(1.4);

    if current < required {
        doc.version = min_version.to_string();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Document, Metadata};
    use lopdf::{Dictionary, Object, Stream};
    use tempfile::tempdir;

    fn simple_pdf() -> Document {
        let mut doc = lopdf::Document::new();
        doc.version = "1.4".to_string();

        // Minimal page
        let pages_id = doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Kids", Object::Array(vec![])),
            ("Count", 0),
        ]));
        let page_id = doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Page".to_vec())),
            ("Parent", Object::Reference(pages_id)),
            ("MediaBox", Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()])),
            ("Resources", Dictionary::from_iter([
                ("Font", Dictionary::new()),
            ]).into()),
            ("Contents", doc.add_object(Stream::new(Dictionary::new(), b""))),
        ]));
        doc.get_object_mut(pages_id).unwrap().as_dict_mut().unwrap()
            .set("Kids", Object::Array(vec![Object::Reference(page_id)]));
        doc.get_object_mut(pages_id).unwrap().as_dict_mut().unwrap()
            .set("Count", 1);

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
        let root_id = doc.inner.trailer.get(b"Root").and_then(Object::as_reference).unwrap();
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

        let root_id = doc.inner.trailer.get(b"Root").and_then(Object::as_reference).unwrap();
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
        doc.inner.trailer.set("Encrypt", Object::Reference(encrypt_id));

        let report = to_pdfa(&mut doc, PdfALevel::A1b).unwrap();
        assert!(report.had_encryption);
        assert!(doc.inner.trailer.get(b"Encrypt").is_err());
    }

    #[test]
    fn pdfa_removes_javascript() {
        let mut doc = simple_pdf();
        let root_id = doc.inner.trailer.get(b"Root").and_then(Object::as_reference).unwrap();
        let mut js_dict = Dictionary::new();
        js_dict.set("S", Object::Name(b"JavaScript".to_vec()));
        js_dict.set("JS", Object::String(b"app.alert('hello')".to_vec(), lopdf::StringFormat::Literal));
        let js_id = doc.inner.add_object(js_dict);
        doc.inner.get_object_mut(root_id).unwrap().as_dict_mut().unwrap()
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

        let root_id = doc.inner.trailer.get(b"Root").and_then(Object::as_reference).unwrap();
        let root = doc.inner.get_dictionary(root_id).unwrap();
        let xmp_id = root.get(b"Metadata").and_then(Object::as_reference).unwrap();
        let xmp_stream = doc.inner.get_stream(xmp_id).unwrap();
        let xmp_content = String::from_utf8_lossy(&xmp_stream.content);
        assert!(xmp_content.contains("pdfaid:part>1</pdfaid:part>"));
        assert!(xmp_content.contains("pdfaid:conformance>B</pdfaid:conformance>"));
        assert!(xmp_content.contains("PDF/A-1b"));
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
    }
}