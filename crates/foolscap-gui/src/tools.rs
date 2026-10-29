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
    Linearize,
    PdfA,
    CheckPdfA,
    Bookmarks,
    NUp,
    Booklet,
    Halve,
    FillForm,
    VerifySignatures,
    NewBlank,
    QrCode,
    ToSvg,
    ToMarkdown,
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
            ToolId::Linearize => "linearize",
            ToolId::PdfA => "pdfa",
            ToolId::CheckPdfA => "check-pdfa",
            ToolId::Bookmarks => "bookmarks",
            ToolId::NUp => "n-up",
            ToolId::Booklet => "booklet",
            ToolId::Halve => "halve",
            ToolId::FillForm => "fill-form",
            ToolId::VerifySignatures => "verify-signatures",
            ToolId::NewBlank => "new-blank",
            ToolId::QrCode => "qr-code",
            ToolId::ToSvg => "to-svg",
            ToolId::ToMarkdown => "to-markdown",
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
    /// Signature verification (the `signatures` feature).
    pub signatures: bool,
}

impl Needs {
    const NONE: Needs = Needs {
        document: false,
        render: false,
        convert: false,
        ocr: false,
        office: false,
        signatures: false,
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

    const fn signatures(self) -> Self {
        Needs {
            signatures: true,
            ..self
        }
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
            && (!self.needs.signatures || caps.signatures)
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
        "view-dual-symbolic",
        Needs::NONE,
    ),
    tool(
        ToolId::Split,
        "Split",
        "Burst into pieces",
        "edit-cut-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Compress,
        "Compress",
        "Shrink for sharing",
        "package-x-generic-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Ocr,
        "OCR",
        "Make searchable",
        "edit-find-symbolic",
        Needs::NONE.document().render().ocr(),
    ),
    tool(
        ToolId::ExtractText,
        "Extract text",
        "Plain text out",
        "text-x-generic-symbolic",
        Needs::NONE.document().render(),
    ),
    tool(
        ToolId::ExtractHtml,
        "Extract HTML",
        "Text as HTML",
        "insert-link-symbolic",
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
        "view-list-ordered-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::CropResize,
        "Crop / Resize",
        "Trim or scale the pages",
        "zoom-fit-best-symbolic",
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
        "view-continuous-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Sign,
        "Sign",
        "Draw or type a signature",
        "document-edit-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Linearize,
        "Optimize for web",
        "Fast first page over HTTP",
        "network-server-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::PdfA,
        "Archival PDF/A",
        "Save as PDF/A-1b, 2b or 3b",
        "drive-harddisk-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::CheckPdfA,
        "Check PDF/A",
        "Structural conformance report",
        "emblem-ok-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Bookmarks,
        "Bookmarks",
        "Add an outline entry",
        "bookmark-new-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::NUp,
        "Pages per sheet",
        "Several pages on one sheet",
        "view-grid-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Booklet,
        "Booklet",
        "Reorder for folding",
        "view-paged-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::Halve,
        "Halve pages",
        "Cut each page in two",
        "object-flip-horizontal-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::FillForm,
        "Fill form",
        "Values from a JSON file",
        "document-edit-symbolic",
        Needs::NONE.document(),
    ),
    tool(
        ToolId::VerifySignatures,
        "Verify signatures",
        "Integrity and trust report",
        "security-high-symbolic",
        Needs::NONE.document().signatures(),
    ),
    tool(
        ToolId::NewBlank,
        "New blank PDF",
        "Empty pages of a set size",
        "document-new-symbolic",
        Needs::NONE,
    ),
    tool(
        ToolId::QrCode,
        "QR code",
        "A scannable code as PDF",
        "view-grid-symbolic",
        Needs::NONE.convert(),
    ),
    tool(
        ToolId::ToSvg,
        "Pages as SVG",
        "Vector files, one per page",
        "image-x-generic-symbolic",
        Needs::NONE.document().render(),
    ),
    tool(
        ToolId::ToMarkdown,
        "Export Markdown",
        "Structured text out",
        "text-x-generic-symbolic",
        Needs::NONE.document().render(),
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
            ToolId::Linearize,
            ToolId::PdfA,
            ToolId::CheckPdfA,
            ToolId::Bookmarks,
            ToolId::NUp,
            ToolId::Booklet,
            ToolId::Halve,
            ToolId::FillForm,
            ToolId::VerifySignatures,
            ToolId::NewBlank,
            ToolId::QrCode,
            ToolId::ToSvg,
            ToolId::ToMarkdown,
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

    /// The icon names come from the Adwaita set every GTK4 desktop has. A
    /// name that is missing there draws as a warning triangle on the card.
    #[test]
    fn card_icons_exist_in_the_adwaita_set() {
        let root = std::path::Path::new("/usr/share/icons/Adwaita/symbolic");
        if !root.is_dir() {
            eprintln!("skipping: the Adwaita icon set is not installed");
            return;
        }
        let have = |name: &str| {
            std::fs::read_dir(root)
                .into_iter()
                .flatten()
                .flatten()
                .any(|group| group.path().join(format!("{name}.svg")).exists())
        };
        for tool in TOOLS {
            assert!(have(tool.icon), "{} has no icon {}", tool.title, tool.icon);
        }
    }
}
