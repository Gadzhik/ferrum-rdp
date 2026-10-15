#!/usr/bin/env bash
# Build Ferrum.app with all Homebrew libraries inside and pack it into a DMG.
# Usage: packaging/macos/make-dmg.sh <version>   (after cargo build --release)
# Needs: brew install dylibbundler
set -euo pipefail
cd "$(dirname "$0")/../.."

version="$1"
brew="$(brew --prefix)"
app=dist/Ferrum.app
contents=$app/Contents

rm -rf dist
mkdir -p "$contents/MacOS" "$contents/Frameworks" "$contents/Resources/share/glib-2.0/schemas"

cp target/release/ferrum "$contents/MacOS/ferrum-bin"

# Launcher: point GLib/GTK at the data shipped inside the bundle.
cat > "$contents/MacOS/ferrum" <<'EOF'
#!/bin/sh
here="$(cd "$(dirname "$0")" && pwd)"
res="$here/../Resources"
export XDG_DATA_DIRS="$res/share"
export GSETTINGS_SCHEMA_DIR="$res/share/glib-2.0/schemas"
exec "$here/ferrum-bin" "$@"
EOF
chmod +x "$contents/MacOS/ferrum"

cat > "$contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Ferrum</string>
  <key>CFBundleDisplayName</key><string>Ferrum</string>
  <key>CFBundleIdentifier</key><string>dev.ferrum.Rdp</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>ferrum</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Remote Desktop File</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>CFBundleTypeExtensions</key><array><string>rdp</string></array>
    </dict>
  </array>
</dict>
</plist>
EOF

# GTK data: settings schemas and icon themes. GTK 4.20+ decodes PNG and
# symbolic SVG icons itself, so no gdk-pixbuf loaders are needed.
cp "$brew"/share/glib-2.0/schemas/*.xml "$contents/Resources/share/glib-2.0/schemas/"
glib-compile-schemas "$contents/Resources/share/glib-2.0/schemas"
mkdir -p "$contents/Resources/share/icons"
for theme in Adwaita hicolor; do
    if [ -d "$brew/share/icons/$theme" ]; then
        cp -R "$brew/share/icons/$theme" "$contents/Resources/share/icons/"
    fi
done

# Copy every Homebrew dylib the binary needs and rewrite install names to
# point inside the bundle.
dylibbundler -of -b -x "$contents/MacOS/ferrum-bin" \
    -d "$contents/Frameworks" -p @executable_path/../Frameworks/

# Unsigned build: ad-hoc signature so arm64 macOS agrees to load it.
codesign --force --deep --sign - "$app"

cp LICENSE README.md dist/
ln -s /Applications dist/Applications
hdiutil create -volname "Ferrum $version" -srcfolder dist -ov -format UDZO \
    "ferrum-$version-macos-arm64.dmg"
