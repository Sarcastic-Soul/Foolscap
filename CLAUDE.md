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

Current state: stage 1 (document manipulation and CLI).

## Commands

```sh
cargo build
cargo run -p foolscap-cli -- <subcommand>     # binary is named `foolscap`
cargo test
cargo test -p pdf-core pages::                # a single module's tests
cargo test --test integration -- merge        # a single integration test by name
cargo fmt
cargo clippy --all-targets -- -D warnings     # must be clean before every commit
```

Cargo lives at `~/.cargo/bin`; non-login shells may need `. "$HOME/.cargo/env"`
first.

## Optional features

Three capabilities are behind non-default Cargo features. This is deliberate:
`render` pulls in `mupdf-rs`, which vendors and builds the MuPDF C sources and
turns a seconds-long build into a ten-to-twenty-minute one. Keeping it opt-in is
what makes ordinary development and CI fast. Do not promote any of these to
`default`.

| Feature | Adds | External requirement |
|---|---|---|
| `render` | Page rendering, thumbnails (MuPDF) | `clang`, `libclang-dev`, `cmake` at build time |
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
- Integration fixtures live in `tests/fixtures/`. Keep each under 100 KB.
- The project is AGPL-3.0-or-later, matching MuPDF. New files do not need a
  license header, but new dependencies must be license-compatible — anything
  linked in must be AGPL-compatible. Tools invoked as subprocesses
  (LibreOffice, Tesseract) are not linked and do not constrain us.

## Traps already identified

- `lopdf` fails opaquely on encrypted PDFs. Detect the `/Encrypt` dictionary and
  return `PdfError::Encrypted` rather than letting a parse error surface.
- Headless LibreOffice collides with itself on concurrent invocations unless
  each call gets an isolated profile via `-env:UserInstallation=`.
- A render cache keyed by `(page, dpi)` is cheap to add in stage 2 and painful to
  retrofit once the GUI is scrolling a document.
