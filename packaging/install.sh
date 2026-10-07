#!/usr/bin/env bash
# Install Foolscap from the portable tarball.
#
#   ./install.sh                 install for this user, into ~/.local
#   sudo ./install.sh            install for everyone, into /usr/local
#   PREFIX=/opt/foolscap ./install.sh
#   ./install.sh --uninstall     remove what an install with the same PREFIX added
#
# Run it from the unpacked tarball. It copies the files under bin/ and share/
# into PREFIX and nothing else, so uninstalling only has to walk the same list.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"

if [[ -z "${PREFIX:-}" ]]; then
    if [[ $EUID -eq 0 ]]; then
        PREFIX=/usr/local
    else
        PREFIX="$HOME/.local"
    fi
fi

desktop=share/applications/com.github.sarcastic_soul.Foolscap.desktop

refresh_caches() {
    # Both are optional: without them the menu entry and icon still appear,
    # just after the next login rather than straight away.
    if command -v update-desktop-database >/dev/null; then
        update-desktop-database -q "$PREFIX/share/applications" 2>/dev/null || true
    fi
    # Only refresh an icon cache that already exists. Creating one where there
    # was none would leave a file that every later icon install there must
    # remember to update.
    if [[ -f "$PREFIX/share/icons/hicolor/icon-theme.cache" ]] &&
        command -v gtk-update-icon-cache >/dev/null; then
        gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    fi
}

if [[ "${1:-}" == "--uninstall" ]]; then
    find bin share -type f | while read -r file; do
        rm -f "$PREFIX/$file"
    done
    refresh_caches
    echo "Removed Foolscap from $PREFIX"
    exit 0
fi

if [[ $# -gt 0 ]]; then
    echo "usage: [PREFIX=dir] $0 [--uninstall]" >&2
    exit 2
fi

find bin -type f | while read -r file; do
    install -Dm755 "$file" "$PREFIX/$file"
done
find share -type f | while read -r file; do
    install -Dm644 "$file" "$PREFIX/$file"
done

# The desktop entry names the binary without a path. ~/.local/bin is not on
# the PATH a desktop session starts with on every distribution, so point the
# installed entry at the exact file instead.
sed -i "s|^Exec=foolscap-gui|Exec=$PREFIX/bin/foolscap-gui|" "$PREFIX/$desktop"

refresh_caches

echo "Installed Foolscap into $PREFIX"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Note: $PREFIX/bin is not on your PATH; add it to run 'foolscap' by name." ;;
esac
echo "LibreOffice and Tesseract are optional. Run 'foolscap capabilities' to see what is available."
