# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Identity

Commits in this repository use `Anish Kumar <anishisbusy@gmail.com>`, which is
already set in the repository's local git config. Do **not** use the
globally-configured email; it belongs to a different account.

The GitHub account is `Sarcastic-Soul`. The remote is
`git@github.com:Sarcastic-Soul/Foolscap.git` over SSH.

## What this is

Foolscap is a Linux-first PDF toolkit: a `pdf-core` library crate holding all
the logic, consumed by a CLI now and a GTK4 desktop application later.
[PLAN.md](PLAN.md) is the staged roadmap and the source of truth for what
belongs in which stage — read it before adding a feature, and update it when
scope changes.

Current state: all seven planned stages are built. 217 tests pass and
`clippy -D warnings` is clean across every feature combination.

## Commands

```sh
# The features are off by default, so most work wants them on.
ALL=--features foolscap-cli/full,pdf-core/render,pdf-core/convert,pdf-core/ocr

cargo build $ALL
cargo run -p foolscap-cli --features full -- <subcommand>   # binary: `foolscap`
cargo run -p foolscap-gui                                   # the GTK4 app
cargo test --workspace $ALL
cargo test -p pdf-core pages::                # one module's unit tests
cargo test -p pdf-core --test compress        # one integration test file
cargo test -p pdf-core --test ocr --features ocr -- --nocapture
cargo fmt
cargo clippy --all-targets --workspace $ALL -- -D warnings   # clean before every commit

./packaging/build-deb.sh                      # produces target/debian/*.deb
```

Cargo lives at `~/.cargo/bin`; non-login shells may need `. "$HOME/.cargo/env"`
first. Building `mupdf-sys` needs `LIBCLANG_PATH=/usr/lib/x86_64-linux-gnu` on
this host, because `libclang-dev` is not installed and `bindgen` will not find
the versioned `libclang-18.so.1` on its own.

Tests that need LibreOffice or Tesseract skip themselves when the tool is
absent, printing why, rather than failing.

Verifying the GUI headlessly needs an explicit backend, or the window is created
but never mapped:

```sh
env -u WAYLAND_DISPLAY xvfb-run -a --server-args="-screen 0 1280x900x24" \
    bash -c 'GDK_BACKEND=x11 GSK_RENDERER=cairo ./target/debug/foolscap-gui file.pdf & \
             sleep 8; xwd -root -silent > shot.xwd; pkill foolscap-gui'
ffmpeg -i shot.xwd shot.png
```

## Optional features

Three capabilities are behind non-default Cargo features, so that a minimal
build has no C toolchain requirement and no external tools. `ocr` implies
`render`, because pages are rasterised before they are recognised. Do not
promote any of these to `default`.

`mupdf` is taken with `default-features = false`, which matters more than it
looks: the default set builds JavaScript, EPUB, HTML and Tesseract support into
MuPDF and turns a twenty-second build into a very long one.

| Feature | Adds | External requirement |
|---|---|---|
| `render` | Page rendering, thumbnails, text extraction (MuPDF) | `clang` at build time; see `LIBCLANG_PATH` above |
| `convert` | Image and Office conversion | LibreOffice on `PATH` at run time |
| `ocr` | Text recognition | Tesseract on `PATH` at run time |

`--features full` on `foolscap-cli` turns on all three. `Capabilities::current()`
reports what a given build supports; the GUI will use it to grey out
unavailable actions rather than failing at invocation time.

## Architecture

The split exists so that the GUI in stage 6 calls the same functions the CLI
calls today. That only holds if `pdf-core` stays free of presentation concerns:

- **No printing.** No `println!` or `eprintln!` anywhere in `pdf-core`.
  Diagnostics go through `tracing`; the CLI installs a subscriber, the GUI will
  install a different one.
- **No exiting.** Nothing in `pdf-core` calls `std::process::exit`.
- **Typed errors.** Every fallible function returns
  `Result<T, PdfError>`. `PdfError` is a `thiserror` enum in
  `crates/pdf-core/src/error.rs` — add a variant rather than stuffing detail
  into a string, because callers branch on these.
- **Progress via callback.** Long operations take
  `Option<&mut dyn FnMut(Progress)>` rather than driving their own display.
- `foolscap-cli` is argument parsing and output formatting only. If you find
  yourself writing PDF logic in `main.rs`, it belongs in `pdf-core`.

Backing libraries are implementation details: `Document` wraps
`lopdf::Document` rather than exposing it, so `lopdf` types should not appear in
public signatures.

### Page indexing

Users type one-indexed, inclusive ranges (`1-3,7,9-`). Everything inside the
library is zero-indexed. `pages::to_zero_indexed` is the single crossing point
between the two, and it bounds-checks. Route conversions through it rather than
subtracting one inline — off-by-one errors here corrupt output silently.

## Conventions

- Any command that writes a file refuses to overwrite an existing path unless
  `--force` is passed.
- `Cargo.lock` is committed; this is an application, not a library for others to
  depend on.
- Integration fixtures are **generated**, not committed: see
  `crates/pdf-core/tests/common/mod.rs`. Page geometry and whether attributes
  are inherited or per-page are exactly what these tests need to vary, and a
  few checked-in files cannot cover the combinations. `tests/fixtures/` is
  reserved for real-world documents that expose something a generator cannot.
- The project is AGPL-3.0-or-later, matching MuPDF. New files do not need a
  license header, but new dependencies must be license-compatible — anything
  linked in must be AGPL-compatible. Tools invoked as subprocesses
  (LibreOffice, Tesseract) are not linked and do not constrain us.

## Traps already identified

- `lopdf` *succeeds* at loading an encrypted PDF — the object structure parses
  fine and every string and stream inside it is ciphertext. `Document::open`
  therefore checks `is_encrypted()` after loading and returns
  `PdfError::Encrypted`; without that check the failure surfaces much later as
  nonsense content.
- Writing a document mutates it: `save` appends a cross-reference stream object.
  Measuring a document's serialised size must therefore be done on a clone, or
  the number will not match the file that is written next.
- Repointing a page at a new parent drops whatever it was inheriting —
  `/Resources`, `/MediaBox`, `/CropBox`, `/Rotate`. `assemble` materialises
  those onto each page first. Everything that rebuilds a page tree must go
  through `crates/pdf-core/src/assemble.rs` rather than reinventing it.
- Headless LibreOffice collides with itself on concurrent invocations unless
  each call gets an isolated profile via `-env:UserInstallation=`. It also needs
  `--infilter=writer_pdf_import` to convert *from* a PDF, and exits zero having
  written nothing when it is missing — so the absence of the output file, not
  the exit status, is the real error signal.
- LibreOffice cannot export a PDF to plain text: its PDF import puts text in
  frames the plain-text exporter ignores. Text extraction goes through MuPDF.
- Pages are rendered without an alpha channel. With one, unmarked areas come
  back transparent and a page reads as floating ink rather than paper.
- In the GUI, do not reach a child widget by walking the tree from its parent.
  That lookup depends on the exact nesting and fails silently when the nesting
  is not what you assumed; hold the widgets in a list instead.
- MuPDF's context is thread-local, so a `PageRenderer` belongs to the thread
  that made it. The GUI keeps one on its worker thread and never shares it.
