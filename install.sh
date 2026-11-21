#!/bin/sh
# Build hearth and install it for the current user:
#   ~/.local/bin/hearth, the .desktop entry and the icon.
# PREFIX=/usr/local sudo ./install.sh installs system-wide instead.
set -eu
cd "$(dirname "$0")"
PREFIX="${PREFIX:-$HOME/.local}"
if [ -n "${HEARTH_BINARY:-}" ]; then
    bin="$HEARTH_BINARY"
else
    nice -n 10 cargo build --release
    bin=target/release/hearth
fi
install -Dm755 "$bin" "$PREFIX/bin/hearth"
install -Dm644 data/dev.hearth.Hearth.svg "$PREFIX/share/icons/hicolor/scalable/apps/dev.hearth.Hearth.svg"
# Absolute Exec path: some compositors don't have ~/.local/bin in PATH.
sed "s|@BIN@|$PREFIX/bin/hearth|" data/dev.hearth.Hearth.desktop > "$PREFIX/share/applications/dev.hearth.Hearth.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
echo "installed $PREFIX/bin/hearth"
