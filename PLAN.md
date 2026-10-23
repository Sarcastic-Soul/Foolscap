# Foolscap — Build Plan

A Linux-first PDF toolkit. Rust core library, CLI first, GTK4 GUI later.

## Locked decisions

| Area | Choice |
|---|---|
| Language | Rust (stable, edition 2021) |
| Architecture | `pdf-core` library crate with no GUI dependencies, consumed by CLI now and GUI later |
| Rendering | MuPDF via `mupdf-rs`, statically linked |
| Document manipulation | `lopdf` (merge, split, rotate, page ops, metadata) |
| PDF generation | `printpdf` (images-to-PDF, simple synthetic documents) |
| Office conversion | LibreOffice headless, invoked as a subprocess |
| OCR | Tesseract, invoked as a subprocess |
| CLI | `clap` (derive API) |
| GUI | `gtk4-rs` |
| Packaging | `.deb` via `cargo-deb` first, then Flatpak, AppImage optional |
| License | AGPL-3.0-or-later |

Subprocess tools (LibreOffice, Tesseract) are not linked, so their licenses do not
constrain distribution of Foolscap itself.

## Environment as of 2026-08-27

Host is Ubuntu 24.04.4 LTS. Rust 1.98.0 is installed via rustup. Still absent,
and needed by later stages: GTK4 development files, LibreOffice, Tesseract,
`libclang-dev`, `cmake`.

## Two deviations from the original build order

**Compression moves from stage 1 to stage 3.** Meaningful PDF size reduction is
mostly image downsampling and recompression, plus stream re-encoding. `lopdf`
alone can drop unused objects, deduplicate identical streams, and re-encode
content streams with Flate — worth having, but typically a single-digit
percentage on real documents. The large wins need a raster path, which arrives
with MuPDF in stage 2. Stage 1 therefore ships `optimize` (lossless object-level
cleanup, honestly named) and stage 3 ships `compress` (image-aware, with quality
levels).

**MuPDF is behind a Cargo feature flag from the start.** `mupdf-rs` vendors and
builds the MuPDF C sources; the first build takes on the order of ten to twenty
minutes and pulls in a C toolchain. Keeping it behind a non-default `render`
feature means stage 1 development, tests, and CI stay in the seconds-to-compile
range, and the heavy dependency only appears when the feature is enabled. Same
pattern for `convert` and `ocr`, which shell out and should not be required for a
minimal build.

## Repository layout

```
Foolscap/
├── Cargo.toml              # workspace manifest
├── Cargo.lock              # committed (this is an application)
├── LICENSE                 # AGPL-3.0
├── README.md
├── PLAN.md
├── rust-toolchain.toml     # pins the toolchain channel
├── .gitignore
├── crates/
│   ├── pdf-core/           # all logic; no GUI, no clap
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── error.rs        # PdfError, Result alias
│   │       ├── document.rs     # Document handle wrapping lopdf::Document
│   │       ├── pages.rs        # PageRange parsing and selection
│   │       └── ops/
│   │           ├── mod.rs
│   │           ├── merge.rs
│   │           ├── split.rs
│   │           ├── rotate.rs
│   │           ├── metadata.rs
│   │           └── optimize.rs
│   └── foolscap-cli/       # thin argument parsing over pdf-core
│       ├── Cargo.toml
│       └── src/main.rs
└── tests/
    └── fixtures/           # small sample PDFs, committed
```

Crates added in later stages: `crates/pdf-core/src/render/`, `.../convert/`,
`.../ocr/`, and `crates/foolscap-gui/`.

## Core API shape

The whole point of the split is that the GUI in stage 6 calls the same functions
the CLI calls in stage 1. That only holds if `pdf-core` never prints, never
exits, and never assumes a terminal.

```rust
// crates/pdf-core/src/lib.rs
pub struct Document { /* wraps lopdf::Document + source path */ }

impl Document {
    pub fn open(path: impl AsRef<Path>) -> Result<Self>;
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<()>;
    pub fn page_count(&self) -> usize;
    pub fn metadata(&self) -> Result<Metadata>;
    pub fn set_metadata(&mut self, m: &Metadata) -> Result<()>;
}

pub fn merge(inputs: &[PathBuf], output: &Path) -> Result<()>;
pub fn split(input: &Path, spec: &SplitSpec, out_dir: &Path) -> Result<Vec<PathBuf>>;
pub fn rotate(doc: &mut Document, pages: &PageRange, degrees: i32) -> Result<()>;
pub fn optimize(doc: &mut Document, level: OptimizeLevel) -> Result<OptimizeReport>;
```

Rules that keep it GUI-ready:

- Every fallible function returns `Result<_, PdfError>`; `PdfError` is a
  `thiserror` enum, never a boxed string.
- No `println!` or `eprintln!` anywhere in `pdf-core`. Use the `tracing` crate;
  the CLI installs a subscriber, the GUI installs a different one.
- Long operations take an optional progress callback
  (`&mut dyn FnMut(Progress)`) rather than driving a progress bar themselves.
- Nothing in `pdf-core` calls `std::process::exit`.

## Stages

Each stage is independently shippable and ends with something runnable.

---

### Stage 0 — Toolchain and skeleton — **done**

**Goal:** `cargo run -p foolscap-cli -- --version` prints a version.

Install Rust via rustup rather than apt (the archive toolchain lags well behind):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Base build dependencies:

```sh
sudo apt install build-essential pkg-config git
```

Then: `git init`, workspace `Cargo.toml` with `members = ["crates/*"]`,
`rust-toolchain.toml` pinning stable, AGPL-3.0 `LICENSE`, `.gitignore` with
`/target`, and the two crates with empty module stubs.

**Done when:** the workspace builds, `cargo test` runs (zero tests), and
`cargo clippy --all-targets -- -D warnings` is clean.

---

### Stage 1 — Document manipulation and CLI — **done**

**Goal:** the tool is already useful without any C dependencies.

Dependencies: `lopdf`, `clap` (derive), `anyhow` (CLI only), `thiserror`,
`tracing`, `tracing-subscriber`.

Commands:

```
foolscap merge a.pdf b.pdf -o out.pdf
foolscap split in.pdf --pages 1-3,7 -o out/          # extract to one file
foolscap split in.pdf --every 1 -o out/              # burst into single pages
foolscap rotate in.pdf --pages 2-4 --degrees 90 -o out.pdf
foolscap info in.pdf                                 # page count, size, metadata
foolscap meta in.pdf --set-title "..." --set-author "..." -o out.pdf
foolscap optimize in.pdf -o out.pdf                  # lossless object cleanup
```

Work items:

1. `PageRange` parser: `1-3,7,9-` and `all`, one-indexed at the boundary,
   zero-indexed internally. Unit-test the parser hard — off-by-one here poisons
   every command above it.
2. `Document` wrapper over `lopdf::Document`, with `open`/`save` and a
   `page_ids()` helper that returns pages in document order.
3. The six operations, each in its own module under `ops/`.
4. CLI subcommands, each one a `clap` struct that maps directly onto a core call.
5. Encrypted-PDF detection. As built: `lopdf` *loads* an encrypted file happily
   and leaves its strings and streams as ciphertext, so `Document::open` checks
   `is_encrypted()` after loading and returns `PdfError::Encrypted`. (Since
   superseded by password handling; see below.)

Three things the implementation added that the plan had not anticipated:

- **`assemble`, a shared page-assembly primitive.** Merge and split are the same
  operation over different page selections. Doing them separately would have
  meant solving attribute inheritance, object renumbering, and page-tree
  rebuilding twice, slightly differently. `crates/pdf-core/src/assemble.rs` is
  the one place that reassembles a page tree.
- **Inherited attribute materialisation.** A page can inherit `/Resources`,
  `/MediaBox`, `/CropBox`, and `/Rotate` from an ancestor. Repointing it at a new
  parent silently drops those, which is how naive merges resize pages. Every
  reassembly materialises them onto the page first.
- **`split_plan`.** The CLI has to know the output names before writing anything
  in order to ask before overwriting. Deriving them in `pdf-core` keeps one
  naming scheme, and a test asserts the plan matches what `split` writes.

**Verified:** 65 tests pass, `clippy -D warnings` is clean, and outputs were
checked with poppler (`pdfinfo`, `pdftotext`) rather than only with the library
that produced them — page counts, page order after reordering extraction, page
geometry across a mixed A4/Letter merge, and a non-ASCII title round trip.

---

### Stage 2 — Rendering — **done**

**Goal:** turn pages into pixels.

Adds the `render` feature, off by default. Extra apt dependencies for the
`mupdf-rs` vendored build:

```sh
sudo apt install clang libclang-dev cmake
```

Commands:

```
foolscap render in.pdf --pages 1 --dpi 150 -o page-%d.png
foolscap thumbnail in.pdf --size 256 -o thumb.png
```

Work items:

1. `render::PageRenderer` — open with MuPDF, render a page at a given DPI or
   fitted to a bounding box, return an RGBA buffer plus dimensions. Return the
   raw buffer, not a PNG: the GUI wants pixels, only the CLI wants a file.
2. PNG encoding in the CLI layer via the `image` crate.
3. A render cache keyed by `(page, dpi)` — trivial to add now, painful to
   retrofit once the GUI is scrolling a document.

**Done when:** rendering page 1 of each fixture at 150 DPI produces a
non-blank image of the expected pixel dimensions, and a cold `cargo build
--features render` succeeds from a clean `target/`.

---

### Stage 3 — Compression — **done**

**Goal:** real size reduction, with honest quality levels.

Commands:

```
foolscap compress in.pdf --level screen|ebook|print -o out.pdf
```

Work items:

1. Walk the page tree for image XObjects; for each, decode, downsample to the
   level's target DPI, re-encode as JPEG (or keep as-is if already smaller), and
   splice the object back in.
2. Preserve alpha-channel images and small images untouched — recompressing a
   50×50 icon costs quality and saves nothing.
3. Re-run stage 1's `optimize` pass afterwards.
4. Report before/after sizes per category (images, streams, structure).

**Done when:** a photo-heavy fixture shrinks by a meaningful margin at `screen`
level and still renders recognizably; a text-only fixture is not made *larger*
by the pass.

---

### Stage 4 — Conversions — **done**

**Goal:** in and out of other formats.

Adds the `convert` feature. Requires LibreOffice on the host:

```sh
sudo apt install libreoffice
```

Commands:

```
foolscap from-images *.jpg -o out.pdf --page-size a4 --fit contain
foolscap to-images in.pdf --format png --dpi 200 -o out/
foolscap from-office report.docx -o report.pdf
foolscap to-office in.pdf --format docx -o out.docx     # best-effort, warn loudly
```

Work items:

1. Images to PDF with `printpdf`: page sizing, fit modes, EXIF orientation.
2. PDF to images: stage 2's renderer plus a batch loop.
3. A `subprocess` helper module — locate the binary on `PATH`, run with a
   timeout, capture stdout/stderr, map a non-zero exit to a typed error. Both
   LibreOffice and Tesseract use it, so build it once, here.
4. LibreOffice needs an isolated profile directory (`-env:UserInstallation=`) or
   concurrent invocations collide and hang. This is the single most common way
   headless LibreOffice integrations break.
5. `to-office` is lossy by nature. Emit a warning on every invocation.

**Done when:** a DOCX round-trips to PDF with correct page count; images-to-PDF
handles mixed orientations; two concurrent `from-office` calls both succeed.

---

### Stage 5 — OCR — **done**

**Goal:** searchable text over scanned pages.

Adds the `ocr` feature.

```sh
sudo apt install tesseract-ocr tesseract-ocr-eng
```

Commands:

```
foolscap ocr in.pdf -o out.pdf --lang eng          # add invisible text layer
foolscap extract-text in.pdf -o out.txt            # embedded text, no OCR
```

Work items:

1. Extract existing embedded text first (MuPDF gives this directly) and skip OCR
   on pages that already have a text layer.
2. For pages needing OCR: render at 300 DPI, pipe to Tesseract with
   `pdf` output, then merge the resulting text layer back over the original page
   so the original image quality is preserved.
3. Language pack detection: list what is installed, fail with a clear message
   naming the missing `tesseract-ocr-<lang>` package.

**Done when:** a scanned fixture becomes text-searchable in an external viewer
while looking visually unchanged.

---

### Stage 6 — GTK4 GUI — **done**

**Goal:** the core is proven; put a window on it.

New crate `crates/foolscap-gui`, depending on `pdf-core` with all features on.
Requires `sudo apt install libgtk-4-dev`.

Scope for the first GUI release, deliberately narrow:

1. Open a document, scrolling page view backed by stage 2's renderer and cache.
2. Thumbnail sidebar with page selection.
3. Page operations on the selection: rotate, delete, reorder by drag.
4. Merge via file drop.
5. Export dialog wrapping compress and to-images.

Architecture notes: all `pdf-core` calls that touch disk run on a worker thread
and report back over a channel; the GTK main loop never blocks. The progress
callback designed in stage 0 is what feeds the progress bars.

**Done when:** a 200-page document opens, scrolls smoothly, and page reordering
saves correctly.

---

### Stage 7 — Packaging — **done**

1. `.deb` via `cargo-deb` (`cargo install cargo-deb`), declaring runtime
   dependencies on `libreoffice` and `tesseract-ocr` as *recommends*, not
   *depends* — the tool works without them, just with fewer commands.
2. Flatpak manifest bundling GTK4 and MuPDF for Fedora, Arch, and the rest.
3. AppImage as an optional portable build.
4. Desktop entry, icon, and man page generated from `clap`.
5. Added afterwards: CI on every push and pull request, and a GitHub release
   for every `v*` tag carrying the `.deb`, a portable tarball with an install
   script, and a Flatpak bundle. The AppImage was dropped in favour of the
   tarball, which covers the same ground with less to maintain.

---

## Conventions to set at stage 0

- `cargo clippy --all-targets -- -D warnings` must pass before every commit.
- `cargo fmt` with default settings, no custom `rustfmt.toml`.
- Integration tests live in `crates/pdf-core/tests/` and generate their fixtures
  rather than committing them, so that page geometry and attribute inheritance
  can be varied per test. `tests/fixtures/` is reserved for real-world documents
  that expose something a generator cannot.
- Every command that writes a file refuses to overwrite an existing path unless
  `--force` is passed.
- Version the CLI surface: the GUI in stage 6 depends on `pdf-core` directly, not
  on shelling out to the CLI.

## Every stage is built

All seven stages are implemented, with 513 tests passing and
`clippy -D warnings` clean across every feature combination.

### What the plan got wrong

Recorded because the corrections are more useful than the original guesses.

- **The MuPDF build is cheap here, not expensive.** With default features off it
  compiles in about twenty seconds, and needs neither `cmake` nor `libclang-dev`:
  `mupdf-sys` drives its vendored sources with `make`, and `bindgen` finds the
  `libclang-18` shared object Ubuntu already ships. The plan budgeted ten to
  twenty minutes and two extra packages.
- **Compression needs no raster backend.** Image XObjects can be decoded and
  resampled with the `image` crate alone, so `compress` works in a default
  build rather than requiring `render`.
- **`lopdf` loads encrypted PDFs quite happily** and hands back ciphertext, so
  the check has to happen after loading rather than instead of it. From 0.44
  it decrypts what it can and returns a near-empty document for what it
  cannot, still with `/Encrypt` in the trailer; the check is now on that.
- **LibreOffice cannot convert a PDF to plain text.** Its Writer PDF import puts
  text in frames that the plain-text exporter ignores, producing an empty file.
  Text extraction goes through MuPDF instead, and `txt` is not offered as a
  conversion target.
- **Pages must be rendered without an alpha channel.** With one, unmarked areas
  come back transparent and a page reads as floating ink rather than paper.

### What the plan did not anticipate

- **`assemble`**, one page-tree rebuilder shared by merge, split, extraction,
  reordering and deletion, including materialising inherited attributes so that
  repointing a page at a new parent does not silently resize it.
- **Content-stream interpretation**, needed to know the size an image is
  actually drawn at before deciding whether it is oversampled.
- **A text-layer graft**, so OCR leaves the original scan untouched.
- **`split_plan` and `convert::plan`**, so the CLI can refuse to overwrite
  before it has already done so.

## Password handling — **done**

The first item from the list below, built after the seven stages.

- **Library.** `Document` and `PageRenderer` both have `open`,
  `open_with_password` and `open_with(path, Option<&str>)`. Plain `open` still
  reads any file whose user password is empty, which is most encrypted PDFs in
  the wild. Errors: `Encrypted` means a password is needed and none was given,
  `WrongPassword` means one was given and did not fit, and
  `UnsupportedEncryption` covers security handlers `lopdf` cannot read.
  `Document::was_encrypted` reports whether a file arrived encrypted.
- **How the password reaches an operation.** Operations that already take an
  options struct (`OcrOptions`, `PdfToImagesOptions`) gained a
  `password: Option<String>` field. The rest take `password: Option<&str>` on
  their full form: `merge_with_progress`, `split_with_progress`, `split_plan`
  and `pdf_to_office`. The short forms (`merge`, `split`) take none. Inputs
  share one password, which files that do not need it ignore — merging a
  protected file with a plain one just works.
- **Output is never encrypted.** A decrypted document loses `/Encrypt`, so every
  save writes a plain file. That includes owner-restricted files that opened
  without a password: editing one removes its restrictions. Re-encrypting on
  save would be a separate feature.
- **`to-office`.** LibreOffice cannot be given a password headlessly, so an
  encrypted input is decrypted into a private (0700) scratch directory first and
  that copy is converted.
- **CLI.** A global `--password`, also read from `FOOLSCAP_PASSWORD` so it can
  stay out of shell history, and a `decrypt` subcommand that refuses a file
  which is not encrypted. `info` reports whether a file was encrypted.
- **Dependency.** `lopdf` went from 0.34 to 0.44 for password authentication and
  AES-256 support, with default features off. Its name accessors now return
  byte strings, which touched `assemble`, `compress`, `placement` and the OCR
  overlay.
- **Tests.** Fixtures are encrypted with RC4-128, AES-128 and AES-256 by
  `lopdf`, and Poppler's `pdftotext` checks that they really are encrypted and
  that the decrypted output reads. Poppler misreads an AES-256 dictionary with
  no `/Length`, which `lopdf` omits, so the fixtures add it.

## Where to go next

Nothing in the original plan is outstanding. The obvious next pieces, in rough
order of value:

1. ~~**Password handling.**~~ Done; see above. Writing encrypted output is the
   remaining half.
2. **Predictor support in `compress`** — **done**. Flate images with a PNG
   predictor (`/Predictor` 10 to 15, all five row filters) or the TIFF
   predictor (`/Predictor 2`) are now un-filtered and go through the same
   resample-and-JPEG path as any other image. The row filters work on bytes,
   so they would handle any depth, but decoding still needs 8-bit components,
   so other depths are skipped as before; the TIFF predictor is only reversed
   at 8 bits. The code is in `crates/pdf-core/src/ops/compress/predictor.rs`.
3. **Tesseract inside the Flatpak**, so OCR works there as it does in the `.deb`.
4. **A page-thumbnail cache on disk**, so reopening a large document is instant.
5. ~~**Undo in the GUI.**~~ Done, along with keyboard shortcuts, plain Save over
   the open file, a prompt for unsaved changes, zoom, an About dialog and a
   password prompt for protected documents.

## Beyond the plan — **done**

Built after the roadmap above, and covered by the same test and clippy gates.

- **Page edits.** `crop` (box, scale, reset), `filter` (recolour images),
  `redact` (by area or by found text; cuts the covered glyphs out of text, including inside Form XObjects, blanks covered image pixels in the image data, and removes small paths; annotations, partly covered paths, inline images and shadings are not touched), `flatten`, `strip`,
  `repair` (rebuilds a damaged cross-reference table and wrong stream lengths).
- **Stamping.** `stamp`, `watermark`, `page-numbers`, `header-footer`.
- **Encryption.** `encrypt` writes AES-256 output with chosen permissions.
- **Conversions.** `from-text`, `from-markdown`, `from-html`, `to-html`, `to-zip`.
- **GUI.** A form and worker handler for each tool above, a job queue with cancel,
  a compare view, and greying out through `Capabilities::current()`.

The GUI crop dialog takes margins in points, millimetres or inches, per page,
with a live preview. Every tool dialog and the compare view have been run and
checked by hand under headless X11.

---

## Stage 8 — Standards & Printing — **planned**

High-value, standards-aligned features that complete the core PDF toolkit for
professional and archival workflows.

### 8.1 Linearize (Web Optimize)
**Goal:** Reorder PDF objects for fast web byte-serving (linearization).

- **CLI:** `foolscap linearize in.pdf -o out.pdf` or `foolscap optimize in.pdf --linearize -o out.pdf`
- **Core:** `lopdf::Document::linearize()` or manual xref/stream reordering
- **GUI:** Export dialog → "Optimize for Web" checkbox
- **Acceptance:** A linearized PDF serves page 1 over HTTP range requests without downloading the whole file.

### 8.2 PDF/A-1b/2b/3b Write
**Goal:** Produce standards-compliant archival PDFs.

- **CLI:** `foolscap to-pdfa in.pdf --level 1b|2b|3b -o out.pdf`
- **Core:** Embed sRGB ICC profile, set OutputIntent, mark `/Metadata` with XMP, ensure fonts embedded, no encryption, no JS, no external refs.
- **Dependencies:** Bundle `sRGB.icc` (tiny, CC0) or use system profile.
- **GUI:** Export dialog → "Save as PDF/A" with level dropdown.
- **Acceptance:** `verapdf` or Adobe Preflight validates the output.

### 8.3 PDF/A Validation
**Goal:** Check whether a PDF meets PDF/A conformance.

- **CLI:** `foolscap check-pdfa in.pdf`
- **Core:** Parse and verify: OutputIntent present, fonts embedded, no prohibited features (encryption, JS, launch actions, external refs, transparency for PDF/A-1), metadata XMP schema.
- **GUI:** Tools → "Validate PDF/A" with detailed report.
- **Acceptance:** Correctly passes known-good PDF/A files, fails on non-conforming ones with actionable errors.

### 8.4 Bookmarks / Outlines
**Goal:** Read, edit, import, export document outline (bookmarks).

- **CLI:**
  - `foolscap bookmark list in.pdf`
  - `foolscap bookmark add in.pdf --parent 0 --title "Chapter 1" --page 5 -o out.pdf`
  - `foolscap bookmark import in.pdf bookmarks.json -o out.pdf`
  - `foolscap bookmark export in.pdf -o bookmarks.json`
- **Core:** `lopdf` reads `/Outlines`; need write support for outline tree with `/Dest` or `/A` actions.
- **GUI:** Sidebar bookmarks panel (click to navigate, drag to reorder, context menu to add/edit/delete).
- **Acceptance:** Round-trip a bookmarked PDF through export/import; GUI panel stays in sync with edits.

### 8.5 n-up (Pages per Sheet)
**Goal:** Place multiple source pages onto one output sheet.

- **CLI:** `foolscap nup in.pdf --layout 2x1|2x2|3x2|... --paper a4 -o out.pdf`
- **Core:** Use `printpdf` to compute layout grid, place each source page as an XObject at calculated position/scale.
- **Options:** Layout (rows×cols), paper size, margins, gap, border, page order (row-major vs column-major).
- **GUI:** Export dialog → "Pages per Sheet" with layout preview.
- **Acceptance:** 4-page A4 → 1-page A4 with 2×2 grid, correct order, readable.

### 8.6 Booklet Imposition
**Goal:** Reorder pages into signatures for fold-and-staple booklet printing.

- **CLI:** `foolscap booklet in.pdf --paper a4 --signature 4 -o out.pdf`
- **Core:** Classic imposition algorithm: for N pages, compute signature order (e.g., 8-page sig: 8,1,2,7,6,3,4,5), then n-up 2×1 on target paper.
- **Options:** Signature size (4, 8, 16), paper size, duplex-aware (short-edge vs long-edge binding).
- **GUI:** Export dialog → "Create Booklet" with signature size and binding options.
- **Acceptance:** 16-page A5 content → 4-sheet A4 booklet, prints duplex, folds to correct page order.

### 8.7 Halve Pages
**Goal:** Cut each page in half (vertical or horizontal), useful for scanned double-page spreads.

- **CLI:** `foolscap halve in.pdf --direction vertical|horizontal -o out.pdf`
- **Core:** For each page, compute two crop boxes (left/right or top/bottom), duplicate page with each crop.
- **Options:** Direction, gap between halves (for gutter), swap order.
- **GUI:** Tools → "Halve Pages" with preview.
- **Acceptance:** A4 landscape scan of two A5 pages → two A5 portrait pages.

### 8.8 Blank PDF Create
**Goal:** Generate a new empty PDF with specified page count and size.

- **CLI:** `foolscap create --pages 5 --size a4 -o blank.pdf`
- **Core:** `printpdf` minimal document with empty pages.
- **Options:** Page count, page size (standard or custom W×H), output path.
- **GUI:** Dashboard → "New Document" card; opens empty document ready for stamps, forms, merge.
- **Acceptance:** `foolscap info blank.pdf` shows 5 A4 pages, zero content.

---

## Stage 9 — Forms & Generation — **planned**

Features that turn Foolscap into a form-filling and programmatic document generation tool.

### 9.1 AcroForm Filling
**Goal:** Fill interactive PDF form fields from structured data.

- **CLI:** `foolscap fill-form form.pdf --data data.json -o filled.pdf`
  - `data.json`: `{"field_name": "value", "checkbox_field": true, "choice_field": "Option 2"}`
  - Support text, checkbox, radio, choice (combo/list), signature fields.
- **Core:** `lopdf` AcroForm API — update `/V` (value), `/DV` (default), `/AS` (appearance state), set `/NeedAppearances` true so viewers regenerate appearances.
- **GUI:** Tools → "Fill Form" dialog: loads form, shows fields in a list, user enters values or loads JSON/CSV, preview, save.
- **Acceptance:** Government/tax PDF forms fill correctly; saved file opens in Adobe Reader with values visible.

### 9.2 QR Code Generator
**Goal:** Create a PDF containing a QR code (optionally with label).

- **CLI:** `foolscap qr-code "https://example.com" --size 50 --label "Scan me" -o qr.pdf`
- **Core:** `qrcodegen` crate → SVG/path → `printpdf` place on page.
- **Options:** Error correction (L/M/Q/H), module size, quiet zone, label text/font, page size.
- **GUI:** Dashboard → "QR Code" card with live preview.
- **Acceptance:** Generated QR scans correctly; label renders below code.

### 9.3 PDF to SVG
**Goal:** Export pages as vector SVG (not rasterized).

- **CLI:** `foolscap to-svg in.pdf --pages 1-3 -o out/`
- **Core:** `mupdf-rs` exposes `page.to_svg()`; write one `.svg` per page.
- **Options:** Page range, output directory, font embedding (reference vs subset).
- **GUI:** Export dialog → "Pages as SVG".
- **Acceptance:** Text remains selectable in browser; vector paths preserved.

### 9.4 PDF to Markdown
**Goal:** Extract structured text as Markdown (headings, lists, tables heuristically).

- **CLI:** `foolscap to-markdown in.pdf -o out.md`
- **Core:** Build on `extract-text` + layout analysis (font size → headings, indentation → lists, grid detection → tables).
- **Options:** Page range, heading detection threshold, table detection on/off.
- **GUI:** Export dialog → "As Markdown".
- **Acceptance:** A structured document (headings, bullets, simple table) produces readable Markdown.

### 9.5 Signature Verification
**Goal:** Validate digital signatures in a PDF (certificate chain, integrity, timestamp).

- **CLI:** `foolscap verify-signatures in.pdf`
- **Core:** Parse `/Sig` dictionaries, extract certificates, verify chain to trust anchor, check signed byte ranges match file, validate timestamp (RFC 3161) if present.
- **Dependencies:** `x509-parser`, `p256`/`rsa`/`sha2` for crypto, `webpki` for trust store.
- **GUI:** Info dialog → "Signatures" tab with status (Valid/Invalid/Unknown), signer, time, certificate details.
- **Acceptance:** Correctly validates PAdES-LTV signatures; detects tampering.

---

## Quick Wins (can land anytime, low effort)

| Feature | CLI | Core | GUI |
|---|---|---|---|
| **Embed/Extract file attachments** | `attach add/extract` | `/EmbeddedFiles` name tree | Tools → Attachments |
| **Layer (OCG) visibility** | `layer list/show/hide` | `/OCProperties` | Layers panel |
| **Page labels** | `page-labels set/get` | `/PageLabels` | Thumbnail tooltip shows label |
| **Document parts (Article threads)** | `article list` | `/Threads` | — |
