#!/usr/bin/env bash
# Build the portable tarball, for distributions the .deb does not cover.
#
# It holds the same files the .deb installs, laid out under bin/ and share/,
# plus install.sh to copy them into ~/.local or /usr/local. The binaries link
# against the system's GTK4 and glibc, so the host needs versions at least as
# new as the machine that built them.
set -euo pipefail

cd "$(dirname "$0")/.."

# `cargo pkgid` prints a URL ending in `#0.1.0` or `#foolscap-cli@0.1.0`.
pkgid="$(cargo pkgid -p foolscap-cli)"
version="${pkgid##*[#@]}"
name="foolscap-$version-$(uname -m)-linux"
out=target/tarball
stage="$out/$name"

# The same build as build-deb.sh, so that running both costs one build: the
# second script finds everything already up to date.
echo "Generating man pages and completions…"
cargo run --quiet -p foolscap-cli --example generate-assets --features full -- dist

echo "Building release binaries…"
cargo build --release -p foolscap-cli --features full
cargo build --release -p foolscap-gui

echo "Staging $name…"
rm -rf "$stage" "$out/$name.tar.gz"
install -Dm755 target/release/foolscap "$stage/bin/foolscap"
install -Dm755 target/release/foolscap-gui "$stage/bin/foolscap-gui"
install -Dm644 -t "$stage/share/applications" \
    packaging/com.github.sarcastic_soul.Foolscap.desktop
install -Dm644 -t "$stage/share/metainfo" \
    packaging/com.github.sarcastic_soul.Foolscap.metainfo.xml
install -Dm644 -t "$stage/share/icons/hicolor/scalable/apps" packaging/foolscap.svg
install -Dm644 -t "$stage/share/man/man1" dist/man/*.1
install -Dm644 dist/completions/foolscap.bash \
    "$stage/share/bash-completion/completions/foolscap"
install -Dm644 -t "$stage/share/zsh/site-functions" dist/completions/_foolscap
install -Dm644 -t "$stage/share/fish/vendor_completions.d" dist/completions/foolscap.fish
install -Dm644 -t "$stage" README.md LICENSE
install -Dm755 -t "$stage" packaging/install.sh

# Fixed owner and file order, so the archive depends only on its contents and
# not on who built it or how the file system happened to list the directory.
tar --sort=name --owner=0 --group=0 --numeric-owner \
    -C "$out" -czf "$out/$name.tar.gz" "$name"

echo
echo "Wrote:"
echo "$out/$name.tar.gz"
