# Packaging

Three ways to ship Foolscap, in the order they are worth reaching for.

## Debian package

```sh
./packaging/build-deb.sh
```

Produces `target/debian/foolscap_<version>_amd64.deb`, containing both binaries,
the desktop entry, the icon, man pages for every subcommand, and shell
completions for bash, zsh and fish.

LibreOffice and Tesseract are **recommended**, not depended on. Without them the
tool still merges, splits, rotates, renders and compresses; it just reports the
office and OCR commands as unavailable. `foolscap capabilities` says which of
them it can actually find.

## Flatpak

```sh
flatpak install flathub org.gnome.Platform//47 org.gnome.Sdk//47 \
    org.freedesktop.Sdk.Extension.rust-stable//24.08
flatpak-builder --user --install --force-clean build-dir \
    packaging/flatpak/com.github.sarcastic_soul.Foolscap.yml
```

The reason to have this as well as the `.deb` is that GTK4 and the vendored
MuPDF build come from the runtime rather than from whatever the host
distribution carries, so Fedora, Arch and Debian all behave identically.

Note that LibreOffice and Tesseract are not on `PATH` inside the sandbox, so
office conversion and OCR are unavailable in a Flatpak build until Tesseract is
added as a module.

## AppImage

Not built here. The `.deb` covers Ubuntu and Debian, the Flatpak covers
everything else, and an AppImage would be a third artefact to keep working for
the sake of portability neither of the other two lacks. If you want one, the
binaries in `target/release` are the whole payload.

## Generated assets

Man pages and completions are generated from the same `clap` definition the
binary uses, so they cannot drift from the real flags:

```sh
cargo run -p foolscap-cli --example generate-assets --features full -- dist
```
