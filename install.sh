#!/usr/bin/env bash
# Build and install Ferrum for the current user (~/.local).
set -euo pipefail
cd "$(dirname "$0")"

if ! pkg-config --exists freerdp3 freerdp-client3 winpr3; then
    echo "FreeRDP 3 development files not found. On Arch: sudo pacman -S freerdp" >&2
    exit 1
fi

cargo build --release -p ferrum

prefix="${PREFIX:-$HOME/.local}"
install -Dm755 target/release/ferrum "$prefix/bin/ferrum"
install -Dm644 crates/ferrum/data/dev.ferrum.Rdp.desktop "$prefix/share/applications/dev.ferrum.Rdp.desktop"
install -Dm644 crates/ferrum/data/ferrum-rdp.xml "$prefix/share/mime/packages/ferrum-rdp.xml"
update-mime-database "$prefix/share/mime" 2>/dev/null || true
update-desktop-database "$prefix/share/applications" 2>/dev/null || true
xdg-mime default dev.ferrum.Rdp.desktop application/x-rdp 2>/dev/null || true

echo "Installed to $prefix/bin/ferrum"
