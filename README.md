# Foolscap

A Linux-first PDF toolkit. Rust core library, command-line interface first,
GTK4 desktop application later.

Foolscap is at stage 0: the workspace is scaffolded and builds, but no PDF
operations are implemented yet. See [PLAN.md](PLAN.md) for the staged roadmap.

## Layout

| Path | What it is |
|---|---|
| `crates/pdf-core` | All PDF logic. No GUI or CLI dependencies. |
| `crates/foolscap-cli` | The `foolscap` binary. Argument parsing over `pdf-core`. |
| `tests/fixtures` | Small sample PDFs used by integration tests. |

The core is deliberately kept free of presentation concerns so that the CLI
today and the GTK front end later call exactly the same functions.

## Build

Requires a stable Rust toolchain and `build-essential`.

```sh
cargo build
cargo run -p foolscap-cli -- --version
cargo run -p foolscap-cli -- capabilities
```

## Optional features

Three capabilities are gated behind Cargo features, off by default. Each pulls
in a heavy dependency or an external program, and none is needed for basic
document manipulation.

| Feature | Adds | Needs |
|---|---|---|
| `render` | Page rendering, thumbnails | MuPDF, built from vendored sources |
| `convert` | Image and Office conversion | LibreOffice on `PATH` |
| `ocr` | Text recognition | Tesseract on `PATH` |

```sh
cargo build --features full          # everything
cargo run -p foolscap-cli --features full -- capabilities
```

The first build with `render` compiles the MuPDF C sources and takes
considerably longer than a normal build.

## Development

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Clippy must be clean before every commit.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
