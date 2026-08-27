#!/usr/bin/env bash
# Build the Debian package.
#
# Everything is built with all features on, because a distribution package that
# silently lacks OCR would be worse than one that recommends the tools it needs.
set -euo pipefail

cd "$(dirname "$0")/.."

command -v cargo-deb >/dev/null || {
    echo "cargo-deb is not installed. Run: cargo install cargo-deb" >&2
    exit 1
}

echo "Generating man pages and completions…"
cargo run --quiet -p foolscap-cli --example generate-assets --features full -- dist

echo "Building release binaries…"
cargo build --release -p foolscap-cli --features full
cargo build --release -p foolscap-gui

echo "Packaging…"
# The binaries are already built, and cargo-deb would otherwise rebuild the CLI
# without the feature flags above.
cargo deb --no-build --no-strip -p foolscap-cli

echo
echo "Wrote:"
ls -1 target/debian/*.deb
