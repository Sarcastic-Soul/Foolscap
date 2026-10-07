# Foolscap

[![CI](https://github.com/Sarcastic-Soul/Foolscap/actions/workflows/ci.yml/badge.svg)](https://github.com/Sarcastic-Soul/Foolscap/actions/workflows/ci.yml)

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

## Install

Download a file from the
[Releases page](https://github.com/Sarcastic-Soul/Foolscap/releases/latest).
Each release has three; pick one.

**Ubuntu 24.04 or newer, Debian 13 or newer** — the `.deb` package:

```sh
sudo apt install ./foolscap_0.1.0_amd64.deb
```

**Any distribution, with Flatpak** — the `Foolscap.flatpak` bundle:

```sh
flatpak install --user Foolscap.flatpak
flatpak run com.github.sarcastic_soul.Foolscap                     # the desktop app
flatpak run --command=foolscap com.github.sarcastic_soul.Foolscap  # the command-line tool
```

**Any distribution, without a package manager** — the tarball. It needs GTK 4.10
or newer and glibc 2.39 or newer (Ubuntu 24.04, Fedora 40 and later are fine):

```sh
tar xzf foolscap-0.1.0-x86_64-linux.tar.gz
cd foolscap-0.1.0-x86_64-linux
./install.sh                 # into ~/.local; use sudo for /usr/local
./install.sh --uninstall     # to remove it again
```

[LibreOffice](https://www.libreoffice.org/) and
[Tesseract](https://github.com/tesseract-ocr/tesseract) are optional. Without
them, Foolscap still merges, splits, rotates, compresses and renders; it just
cannot convert office files or recognise text in scans. Install them from your
distribution to turn those on (`sudo apt install libreoffice tesseract-ocr` on
Ubuntu), and run `foolscap capabilities` to see what it can find. The Flatpak
cannot reach either of them yet.

`SHA256SUMS` on the same page lets you check a download:
`sha256sum --check --ignore-missing SHA256SUMS`.

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
./packaging/build-deb.sh        # target/debian/foolscap_<version>_amd64.deb
./packaging/build-tarball.sh    # target/tarball/foolscap-<version>-x86_64-linux.tar.gz
```

See [packaging/README.md](packaging/README.md) for the Flatpak and for how a
release is made.

## License

AGPL-3.0-or-later, matching MuPDF. See [LICENSE](LICENSE).

LibreOffice and Tesseract are run as subprocesses, never linked, so their
licences do not reach Foolscap.
