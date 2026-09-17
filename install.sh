#!/bin/bash

# DROSTJE. Installer
# This script builds a macOS .app bundle and installs it to /Applications or a custom directory.

set -e

APP_NAME="Drostje"
BINARY_NAME="drostje"
INSTALL_DIR="${INSTALL_DIR:-/Applications}"
BUNDLE_NAME="${APP_NAME}.app"
BUILD_DIR="target/release"

echo "--- Installing $APP_NAME ---"

# 1. Check requirements
if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo is not installed. Please install Rust and Cargo first." >&2
    exit 1
fi

# 2. Get version from Cargo.toml
VERSION=$(grep '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/')
if [ -z "$VERSION" ]; then
    VERSION="0.1.0"
    echo "Warning: Could not determine version from Cargo.toml. Defaulting to 0.1.0"
fi

echo "Detected version: $VERSION"

# 3. Build release binary
echo "Step 1: Building release binary..."
cargo build --release

# 4. Create App Bundle
echo "Step 2: Creating macOS app bundle..."
DIST_DIR="dist"
APP_BUNDLE="$DIST_DIR/$BUNDLE_NAME"

# Clean previous builds
rm -rf "$DIST_DIR"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

# Copy binary
cp "$BUILD_DIR/$BINARY_NAME" "$APP_BUNDLE/Contents/MacOS/"

# Create Info.plist
cat <<EOF > "$APP_BUNDLE/Contents/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>$BINARY_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>com.drostje.app</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
</dict>
</plist>
EOF

# Build the .icns icon set. Finder, Launchpad and Spotlight only read .icns declared
# via CFBundleIconFile; a bare PNG in Resources is ignored (the Dock icon comes from
# the binary itself at runtime, which is why it showed even without this).
ICON_SRC="assets/drostje-app-icon.png"
if [ -f "$ICON_SRC" ]; then
    echo "Adding app icon..."
    ICONSET="$DIST_DIR/AppIcon.iconset"
    mkdir -p "$ICONSET"
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
        double=$((size * 2))
        sips -z "$double" "$double" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns "$ICONSET" -o "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
    rm -rf "$ICONSET"
fi

# 5. Install
echo "Step 3: Installing to $INSTALL_DIR..."

# Check if target directory is writable
if [ ! -w "$INSTALL_DIR" ]; then
    echo "Target directory $INSTALL_DIR is not writable. Requesting sudo..."
    sudo cp -R "$APP_BUNDLE" "$INSTALL_DIR/"
else
    cp -R "$APP_BUNDLE" "$INSTALL_DIR/"
fi

# Finder caches bundle icons; bump the mtime so it re-reads Info.plist and the .icns.
touch "$INSTALL_DIR/$BUNDLE_NAME"

# Cleanup build artifacts (optional, user might want to keep them for dev)
# rm -rf "$DIST_DIR"

echo "--- Installation Complete! ---"
echo "You can find $APP_NAME in $INSTALL_DIR"
echo "To run from terminal: $INSTALL_DIR/$BUNDLE_NAME/Contents/MacOS/$BINARY_NAME"
