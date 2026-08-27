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

Host is Ubuntu 24.04.4 LTS. Present: `git`, `cc`, `clang`, `pkg-config`.
Absent: `cargo`, `rustc`, GTK4 development files, LibreOffice, Tesseract.
Stage 0 installs all of these.

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

### Stage 0 — Toolchain and skeleton

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

### Stage 1 — Document manipulation and CLI

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
5. Encrypted-PDF detection: `lopdf` fails opaquely on encrypted files. Detect the
   `/Encrypt` dictionary and return a specific `PdfError::Encrypted` so the CLI
   can say so plainly.

**Done when:** each command round-trips a fixture PDF and the output opens
correctly in an external viewer; `merge` of N files produces exactly the sum of
their page counts; `split --every 1` followed by `merge` reproduces the original
page count.

---

### Stage 2 — Rendering

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

### Stage 3 — Compression

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

### Stage 4 — Conversions

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

### Stage 5 — OCR

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

### Stage 6 — GTK4 GUI

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

### Stage 7 — Packaging

1. `.deb` via `cargo-deb` (`cargo install cargo-deb`), declaring runtime
   dependencies on `libreoffice` and `tesseract-ocr` as *recommends*, not
   *depends* — the tool works without them, just with fewer commands.
2. Flatpak manifest bundling GTK4 and MuPDF for Fedora, Arch, and the rest.
3. AppImage as an optional portable build.
4. Desktop entry, icon, and man page generated from `clap`.

---

## Conventions to set at stage 0

- `cargo clippy --all-targets -- -D warnings` must pass before every commit.
- `cargo fmt` with default settings, no custom `rustfmt.toml`.
- Integration tests live in `crates/pdf-core/tests/` and use committed fixture
  PDFs; keep each fixture under 100 KB so the repository stays small.
- Every command that writes a file refuses to overwrite an existing path unless
  `--force` is passed.
- Version the CLI surface: the GUI in stage 6 depends on `pdf-core` directly, not
  on shelling out to the CLI.

## Immediate next step

Stage 0: install rustup and the base build dependencies, then scaffold the
workspace, both crates, the license, and the module stubs so
`cargo run -p foolscap-cli -- --version` works.
