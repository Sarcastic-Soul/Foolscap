# Packaging

Three ways to ship Foolscap, in the order they are worth reaching for. All
three are built and attached to a GitHub release by
[`.github/workflows/release.yml`](../.github/workflows/release.yml) whenever a
version tag is pushed; see [Making a release](#making-a-release).

## Debian package

```sh
cargo install cargo-deb
./packaging/build-deb.sh
```

Produces `target/debian/foolscap_<version>_amd64.deb`, containing both binaries,
the desktop entry, the metainfo file, the icon, man pages for every subcommand,
and shell completions for bash, zsh and fish.

LibreOffice and Tesseract are **recommended**, not depended on. Without them the
tool still merges, splits, rotates, renders and compresses; it just reports the
office and OCR commands as unavailable. `foolscap capabilities` says which of
them it can actually find.

## Tarball

```sh
./packaging/build-tarball.sh
```

Produces `target/tarball/foolscap-<version>-x86_64-linux.tar.gz`, holding the
same files as the `.deb` under `bin/` and `share/`, plus the README, the
licence, and [`install.sh`](install.sh). That script copies the files into
`~/.local`, or `/usr/local` when run as root, or any `PREFIX=` you give it, and
`./install.sh --uninstall` removes them again.

The binaries link against the system's GTK4 and glibc, so they only run where
both are at least as new as on the machine that built them. Release builds come
from Ubuntu 24.04: GTK 4.14 and glibc 2.39.

## Flatpak

```sh
flatpak install flathub org.gnome.Platform//50 org.gnome.Sdk//50 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08 \
    org.freedesktop.Sdk.Extension.llvm20//25.08
flatpak-builder --user --install --force-clean build-dir \
    packaging/flatpak/com.github.sarcastic_soul.Foolscap.yml
```

Or build a single file to hand to someone else:

```sh
flatpak-builder --repo=repo --force-clean build-dir \
    packaging/flatpak/com.github.sarcastic_soul.Foolscap.yml
flatpak build-bundle repo Foolscap.flatpak com.github.sarcastic_soul.Foolscap
```

The reason to have this as well as the `.deb` is that GTK4 and the vendored
MuPDF build come from the runtime rather than from whatever the host
distribution carries, so Fedora, Arch and Debian all behave identically.

Tesseract is built into the Flatpak from pinned sources (Leptonica 1.87.0,
Tesseract 5.5.4, English `tessdata_fast` 4.1.0), so OCR works there. LibreOffice
is not bundled, so office conversion is unavailable in the Flatpak.

### Crate sources

`flatpak-builder` builds without network access, so Cargo cannot download
crates during the build. Instead
[`flatpak/cargo-sources.json`](flatpak/cargo-sources.json) lists every crate in
`Cargo.lock` with its download address and checksum; `flatpak-builder` fetches
them beforehand and unpacks them where `cargo --offline` will look.

That file is generated, and **must be regenerated whenever `Cargo.lock`
changes**. CI fails if it is out of date. To regenerate it:

```sh
curl -LO https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/41c20aa10819cdb2a4f3ca171758a96d1955c018/cargo/flatpak-cargo-generator.py
python3 -m venv .venv && .venv/bin/pip install aiohttp PyYAML tomlkit
.venv/bin/python flatpak-cargo-generator.py Cargo.lock \
    -o packaging/flatpak/cargo-sources.json
rm -r .venv flatpak-cargo-generator.py
```

The script is pinned to one commit so that CI and a local run produce the same
file. If you move it to a newer commit, change the address in
[`ci.yml`](../.github/workflows/ci.yml) as well.

### Names

The desktop entry and metainfo file are named after the application id,
`com.github.sarcastic_soul.Foolscap`, which is also what the GUI registers.
The icon is `foolscap.svg` in the source tree and in the `.deb`; Flatpak only
accepts icons named after the application id, so the manifest's
`rename-icon: foolscap` renames it during the build and rewrites the desktop
entry's `Icon=` line to match.

## AppImage

Not built here. The `.deb` covers Ubuntu and Debian, the Flatpak and the tarball
cover everything else, and an AppImage would be another artefact to keep
working for the sake of portability the others already give.

## Generated assets

Man pages and completions are generated from the same `clap` definition the
binary uses, so they cannot drift from the real flags:

```sh
cargo run -p foolscap-cli --example generate-assets --features full -- dist
```

Outputs `dist/man/foolscap.1` plus `foolscap-<subcommand>.1` for each subcommand, and `dist/completions/foolscap.{bash,zsh,fish}`.

## Making a release

1. Bump `version` under `[workspace.package]` in the top-level `Cargo.toml`,
   then run `cargo build` so `Cargo.lock` picks up the new number.
2. Add a `<release version="X.Y.Z" date="YYYY-MM-DD">` entry to
   [`com.github.sarcastic_soul.Foolscap.metainfo.xml`](com.github.sarcastic_soul.Foolscap.metainfo.xml).
3. Commit, and push to `main`. Wait for CI to pass.
4. Tag and push the tag:

   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

The release workflow then checks that the tag matches the version in
`Cargo.toml` and that the metainfo has an entry for it, stopping before it
builds anything if either is wrong. It builds the `.deb`, the tarball and the
Flatpak bundle, writes `SHA256SUMS` for them, and publishes a GitHub release
with all four attached. A tag with a hyphen in it, such as `v0.2.0-rc1`, is
published as a pre-release.
