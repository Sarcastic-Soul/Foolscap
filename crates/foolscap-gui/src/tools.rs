//! The dashboard tool registry: one card per operation.
//!
//! The dashboard is a catalogue, not logic. Each entry says what the tool
//! needs — an open document, Tesseract, LibreOffice — so the window can grey
//! the card out instead of failing when it is pressed. Availability comes
//! from [`pdf_core::Capabilities::current`], [`pdf_core::ocr::is_available`]
//! and [`pdf_core::convert::office::is_available`]; core-team agents adding an
//! operation add one row here and one worker arm, nothing else moves.

use pdf_core::Capabilities;

/// A dashboard tool, identified stably for actions and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolId {
    Merge,
    Split,
    Compress,
    Ocr,
    ExtractText,
    ExtractHtml,
    ImagesToPdf,
    PdfToImages,
    OfficeToPdf,
    PdfToOffice,
    FromText,
    FromMarkdown,
    Encrypt,
    Watermark,
    PageNumbers,
    CropResize,
    Redact,
    Flatten,
    Sign,
}

impl ToolId {
    /// The window action behind this card, without the `win.` prefix.
    pub const fn action(self) -> &'static str {
        match self {
            ToolId::Merge => "merge",
            ToolId::Split => "split",
            ToolId::Compress => "compress-screen",
            ToolId::Ocr => "ocr",
            ToolId::ExtractText => "export-text",
            ToolId::ExtractHtml => "export-html",
            ToolId::ImagesToPdf => "from-images",
            ToolId::PdfToImages => "export-images",
            ToolId::OfficeToPdf => "from-office",
            ToolId::PdfToOffice => "pdf-to-office",
            ToolId::FromText => "from-text",
            ToolId::FromMarkdown => "from-markdown",
            ToolId::Encrypt => "encrypt",
            ToolId::Watermark => "stamp",
            ToolId::PageNumbers => "page-numbers",
            ToolId::CropResize => "crop",
            ToolId::Redact => "redact",
            ToolId::Flatten => "flatten",
            ToolId::Sign => "sign",
        }
    }
}

/// What a tool needs before it can run. Built with the `const` helpers below
/// so each card in [`TOOLS`] reads as one short line of intent.
#[derive(Debug, Clone, Copy)]
pub struct Needs {
    /// Reads the open document (as opposed to creating one).
    pub document: bool,
    /// Page rendering or text extraction (MuPDF, the `render` feature).
    pub render: bool,
    /// Format conversion (the `convert` feature).
    pub convert: bool,
    /// Tesseract on `PATH`.
    pub ocr: bool,
    /// LibreOffice on `PATH`.
    pub office: bool,
}

impl Needs {
    const NONE: Needs = Needs {
        document: false,
        render: false,
        convert: false,
        ocr: false,
        office: false,
    };

    /// Reads the open document.
    const fn document(self) -> Self {
        Needs {
            document: true,
            ..self
        }
    }

    const fn render(self) -> Self {
        Needs {
            render: true,
            ..self
        }
    }

    const fn convert(self) -> Self {
        Needs {
            convert: true,
            ..self
        }
    }

    const fn ocr(self) -> Self {
        Needs { ocr: true, ..self }
    }

    /// LibreOffice also means the `convert` feature, which wraps it.
    const fn office(self) -> Self {
        Needs {
            office: true,
            convert: true,
            ..self
        }
    }
}

/// One dashboard card.
#[derive(Debug, Clone, Copy)]
pub struct Tool {
    pub id: ToolId,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub icon: &'static str,
    pub needs: Needs,
}

impl Tool {
    /// True when this build can run the tool at all.
    pub fn is_supported(&self) -> bool {
        let caps = Capabilities::current();
        (!self.needs.render || caps.render)
            && (!self.needs.convert || caps.convert)
            && (!self.needs.ocr || caps.ocr)
    }

    /// Whether the card is clickable right now.
    pub fn enabled_for(&self, doc_open: bool) -> bool {
        self.disabled_reason(doc_open).is_none()
    }

    /// Why a disabled card is disabled, for the tooltip.
    pub fn disabled_reason(&self, doc_open: bool) -> Option<&'static str> {
        if !self.is_supported() {
            return Some("Not included in this build");
        }
        if self.needs.ocr && !pdf_core::ocr::is_available() {
            return Some("Needs Tesseract: sudo apt install tesseract-ocr");
        }
        if self.needs.office && !pdf_core::convert::office::is_available() {
            return Some("Needs LibreOffice to be installed");
        }
        if self.needs.document && !doc_open {
            return Some("Open a PDF first");
        }
        None
    }
}

const fn tool(
    id: ToolId,
    title: &'static str,
    subtitle: &'static str,
    icon: &'static str,
    needs: Needs,
) -> Tool {
    Tool {
        id,
        title,
        subtitle,
        icon,
        needs,
    }
}

/// Every dashboard card, in display order.
pub const TOOLS: &[Tool] = &[
    tool(
        ToolId::Merge,
        "Merge",
        "Combine PDFs",
        "document-merge-symbolic",
        Needs::NONE,
    ),
    tool(
        ToolId::Split,
        "Split",
        "Burst into pieces",
        "document-split-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Compress,
        "Compress",
        "Shrink for sharing",
        "package-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Ocr,
        "OCR",
        "Make searchable",
        "document-properties-symbolic",
        Needs::NONE.document().render().ocr(),
    ),
    tool(
        ToolId::ExtractText,
        "Extract text",
        "Plain text out",
        "document-edit-symbolic",
        Needs::NONE.document().render(),
    ),
    tool(
        ToolId::ExtractHtml,
        "Extract HTML",
        "Text as HTML",
        "text-html-symbolic",
        Needs::NONE.document().render().convert(),
    ),
    tool(
        ToolId::ImagesToPdf,
        "Images to PDF",
        "One page per image",
        "image-x-generic-symbolic",
        Needs::NONE.convert(),
    ),
    tool(
        ToolId::PdfToImages,
        "PDF to images",
        "Pages as PNG, JPEG or ZIP",
        "image-x-generic-symbolic",
        Needs::NONE.document().render().convert(),
    ),
    tool(
        ToolId::OfficeToPdf,
        "Office to PDF",
        "Word, ODT and more",
        "x-office-document-symbolic",
        Needs::NONE.office(),
    ),
    tool(
        ToolId::PdfToOffice,
        "PDF to Office",
        "Approximate DOCX, PPTX, XLSX",
        "x-office-document-symbolic",
        Needs::NONE.document().office(),
    ),
    tool(
        ToolId::FromText,
        "New from text",
        "Plain text in",
        "document-new-symbolic",
        Needs::NONE.convert(),
    ),
    tool(
        ToolId::FromMarkdown,
        "New from Markdown",
        "Rendered to PDF",
        "document-new-symbolic",
        Needs::NONE.convert(),
    ),
    tool(
        ToolId::Encrypt,
        "Encrypt",
        "Password-protect",
        "dialog-password-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Watermark,
        "Watermark",
        "Stamp every page",
        "insert-text-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::PageNumbers,
        "Page numbers",
        "Folio the pages",
        "format-ordered-list-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::CropResize,
        "Crop / Resize",
        "Trim or scale the pages",
        "object-crop-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Redact,
        "Redact",
        "Burn text or regions out",
        "edit-delete-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Flatten,
        "Flatten",
        "Bake forms in",
        "document-flatten-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Sign,
        "Sign",
        "Draw or type a signature",
        "edit-signature-symbolic",
        Needs::NONE.document(),
    ),
];

/// Find a tool by its window action name.
#[cfg(test)]
pub fn by_action(action: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.id.action() == action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_maps_to_exactly_one_card() {
        for tool in TOOLS {
            let found = by_action(tool.id.action());
            assert!(found.is_some(), "no card for {}", tool.id.action());
            assert_eq!(found.unwrap().id, tool.id);
        }
    }

    #[test]
    fn actions_are_distinct_so_accels_do_not_collide() {
        let mut seen = std::collections::HashSet::new();
        for tool in TOOLS {
            assert!(
                seen.insert(tool.id.action()),
                "duplicate {}",
                tool.id.action()
            );
        }
    }

    #[test]
    fn registry_covers_the_dashboard_brief() {
        let ids: Vec<ToolId> = TOOLS.iter().map(|tool| tool.id).collect();
        for required in [
            ToolId::Merge,
            ToolId::Split,
            ToolId::Compress,
            ToolId::Ocr,
            ToolId::ExtractText,
            ToolId::ExtractHtml,
            ToolId::ImagesToPdf,
            ToolId::PdfToImages,
            ToolId::OfficeToPdf,
            ToolId::PdfToOffice,
            ToolId::FromText,
            ToolId::FromMarkdown,
            ToolId::Encrypt,
            ToolId::Watermark,
            ToolId::PageNumbers,
            ToolId::CropResize,
            ToolId::Redact,
            ToolId::Flatten,
            ToolId::Sign,
        ] {
            assert!(ids.contains(&required), "missing {required:?}");
        }
    }

    #[test]
    fn a_tool_needing_a_document_is_disabled_without_one() {
        let merge = TOOLS.iter().find(|t| t.id == ToolId::Merge).unwrap();
        let split = TOOLS.iter().find(|t| t.id == ToolId::Split).unwrap();
        assert!(merge.enabled_for(false), "merge works with no document");
        assert!(!split.enabled_for(false));
        assert!(split.enabled_for(true) || split.disabled_reason(true).is_some());
    }

    #[test]
    fn disabled_cards_explain_themselves() {
        let split = TOOLS.iter().find(|t| t.id == ToolId::Split).unwrap();
        assert_eq!(split.disabled_reason(false), Some("Open a PDF first"));
        assert_eq!(split.disabled_reason(true), None);
    }
}
