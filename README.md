# Foolscap

A Linux-first PDF toolkit: a Rust core library with a command-line tool and a
GTK4 desktop application over it.

```sh
foolscap merge a.pdf b.pdf -o out.pdf
foolscap split report.pdf --pages 1-3,7 -o out/
foolscap compress scan.pdf --level screen -o small.pdf
foolscap ocr scan.pdf -o searchable.pdf
foolscap-gui report.pdf
```

See [PLAN.md](PLAN.md) for how it was built and what is worth doing next.

## Layout

| Path | What it is |
|---|---|
| `crates/pdf-core` | All PDF logic. No GUI or CLI dependencies. |
| `crates/foolscap-cli` | The `foolscap` command-line tool. |
| `crates/foolscap-gui` | The `foolscap-gui` GTK4 application. |
| `packaging/` | Debian and Flatpak packaging. |

The core is kept free of presentation concerns — it never prints, never exits,
and reports progress through a callback — so the CLI and the desktop
application call exactly the same functions.

## Build

Requires a stable Rust toolchain and `build-essential`.

```sh
cargo build --features foolscap-cli/full
cargo run -p foolscap-cli --features full -- capabilities
cargo run -p foolscap-gui
```

`foolscap capabilities` reports which optional features this build has and
whether the external tools they need are actually installed.

## Optional features

Three capabilities are gated behind Cargo features, off by default. Each pulls
in a heavy dependency or an external program, and none is needed for basic
document manipulation.

| Feature | Adds | Needs |
|---|---|---|
| `render` | Page rendering, thumbnails, text extraction | MuPDF, built from vendored sources |
| `convert` | Image and Office conversion | LibreOffice on `PATH` |
| `ocr` | Text recognition | Tesseract on `PATH` |

```sh
cargo build --features full          # everything
cargo run -p foolscap-cli --features full -- capabilities
```

The first build with `render` compiles the MuPDF C sources, which takes about
twenty seconds on top of a normal build. `ocr` implies `render`, since pages are
rasterised before they are recognised.

## What it does

| Command | |
|---|---|
| `merge`, `split`, `rotate`, `delete`, `reorder` | page operations |
| `info`, `meta` | read and edit document metadata |
| `optimize` | lossless cleanup |
| `compress` | resample images to a target resolution; lossy |
| `render`, `thumbnail`, `to-images` | rasterise pages |
| `from-images` | bind images into a document |
| `from-office`, `to-office` | LibreOffice conversion |
| `ocr` | add an invisible text layer to a scan |
| `extract-text` | read out the text a document already carries |

## Development

```sh
ALL=--features foolscap-cli/full,pdf-core/render,pdf-core/convert,pdf-core/ocr

cargo fmt
cargo clippy --all-targets --workspace $ALL -- -D warnings
cargo test --workspace $ALL
```

Clippy must be clean before every commit. Tests that need LibreOffice or
Tesseract skip themselves when the tool is absent.

## Packaging

```sh
./packaging/build-deb.sh
```

See [packaging/README.md](packaging/README.md) for the Flatpak.

## License

AGPL-3.0-or-later, matching MuPDF. See [LICENSE](LICENSE).

LibreOffice and Tesseract are run as subprocesses, never linked, so their
licences do not reach Foolscap.
