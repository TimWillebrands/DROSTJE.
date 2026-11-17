#!/bin/bash

set -e

APP_NAME="DROSTJE."
BINARY_NAME="drostje"
BUILD_DIR="target/release"
ICON_SRC="assets/drostje-app-icon.png"

echo "--- Installing $APP_NAME ---"

if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo is not installed. Please install Rust and Cargo first." >&2
    exit 1
fi

for tool in cmake pkg-config python3 make curl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Error: Servo requires $tool. Install the native build prerequisites, then rerun ./install.sh." >&2
        exit 1
    fi
done
if [ "$(uname -s)" = Darwin ]; then
    if ! xcrun --show-sdk-path >/dev/null 2>&1; then
        echo "Error: Servo requires the macOS SDK. Install/select Xcode command-line tools with xcode-select." >&2
        exit 1
    fi
fi

VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
if [ -z "$VERSION" ]; then
    VERSION="0.1.0"
    echo "Warning: Could not determine version from Cargo.toml. Defaulting to 0.1.0"
fi

echo "Detected version: $VERSION"

echo "Step 1: Building application with in-process Servo..."
if ! cargo build --release --locked; then
    echo "Error: Application build failed; nothing was installed." >&2
    echo "SpiderMonkey normally downloads a platform-specific prebuilt archive from GitHub." >&2
    echo "A source build requires LLVM/Clang 19 or newer, libclang, Python 3, make, CMake and pkg-config." >&2
    echo "Apple Clang 17 is insufficient for a SpiderMonkey source build. Select an installed LLVM >=19 with CC, CXX and LIBCLANG_PATH, or restore access to GitHub release downloads." >&2
    echo "Linux also needs fontconfig development files and Mesa EGL/GL software drivers. See the preceding native build error for the missing library." >&2
    exit 1
fi

as_owner() {
    if [ "$NEED_SUDO" = 1 ]; then
        sudo "$@"
    else
        "$@"
    fi
}

install_macos() {
    INSTALL_DIR="${INSTALL_DIR:-/Applications}"
    BUNDLE_NAME="DROSTJE.app"
    LEGACY_BUNDLE_NAME="Drostje.app"

    echo "Step 2: Creating macOS app bundle..."
    DIST_DIR="dist"
    APP_BUNDLE="$DIST_DIR/$BUNDLE_NAME"

    rm -rf "$DIST_DIR"
    mkdir -p "$APP_BUNDLE/Contents/MacOS"
    mkdir -p "$APP_BUNDLE/Contents/Resources"

    cp "$BUILD_DIR/$BINARY_NAME" "$APP_BUNDLE/Contents/MacOS/"

    cat <<PLIST > "$APP_BUNDLE/Contents/Info.plist"
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
    <key>CFBundleDisplayName</key>
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
PLIST

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

    echo "Step 3: Installing to $INSTALL_DIR..."
    NEED_SUDO=0
    if [ ! -w "$INSTALL_DIR" ]; then
        echo "Target directory $INSTALL_DIR is not writable. Requesting sudo..."
        NEED_SUDO=1
    fi

    as_owner rm -rf "$INSTALL_DIR/$BUNDLE_NAME" "$INSTALL_DIR/$LEGACY_BUNDLE_NAME"
    as_owner cp -R "$APP_BUNDLE" "$INSTALL_DIR/"

    as_owner touch "$INSTALL_DIR/$BUNDLE_NAME"

    echo "--- Installation Complete! ---"
    echo "You can find $APP_NAME in $INSTALL_DIR"
    echo "To run from terminal: $INSTALL_DIR/$BUNDLE_NAME/Contents/MacOS/$BINARY_NAME"
}

install_linux() {
    INSTALL_DIR="${INSTALL_DIR:-$HOME/.local}"
    BIN_DIR="$INSTALL_DIR/bin"
    SHARE_DIR="$INSTALL_DIR/share"
    APPLICATIONS_DIR="$SHARE_DIR/applications"
    ICON_ROOT="$SHARE_DIR/icons/hicolor"

    echo "Step 2: Installing to $INSTALL_DIR..."
    NEED_SUDO=0
    if ! mkdir -p "$BIN_DIR" "$APPLICATIONS_DIR" 2>/dev/null || [ ! -w "$BIN_DIR" ]; then
        echo "Prefix $INSTALL_DIR is not writable. Requesting sudo..."
        NEED_SUDO=1
        sudo mkdir -p "$BIN_DIR" "$APPLICATIONS_DIR"
    fi

    as_owner install -m 755 "$BUILD_DIR/$BINARY_NAME" "$BIN_DIR/$BINARY_NAME"

    if [ -f "$ICON_SRC" ]; then
        echo "Adding app icon..."
        if command -v magick >/dev/null 2>&1; then
            for size in 16 32 48 64 128 256 512; do
                target="$ICON_ROOT/${size}x${size}/apps"
                as_owner mkdir -p "$target"
                magick "$ICON_SRC" -resize "${size}x${size}" "png:-" \
                    | as_owner tee "$target/$BINARY_NAME.png" >/dev/null
            done
        else
            echo "ImageMagick (magick) not found; installing the 1024 px icon unscaled."
            target="$ICON_ROOT/512x512/apps"
            as_owner mkdir -p "$target"
            as_owner install -m 644 "$ICON_SRC" "$target/$BINARY_NAME.png"
        fi
    fi

    echo "Adding desktop entry..."
    cat <<DESKTOP | as_owner tee "$APPLICATIONS_DIR/$BINARY_NAME.desktop" >/dev/null
[Desktop Entry]
Type=Application
Version=1.5
Name=$APP_NAME
Comment=Drive local coding agents and dispatch work to the Compliance Software Factory
Exec="$BIN_DIR/$BINARY_NAME"
Icon=$BINARY_NAME
Terminal=false
Categories=Development;IDE;
StartupWMClass=$BINARY_NAME
DESKTOP
    as_owner chmod 644 "$APPLICATIONS_DIR/$BINARY_NAME.desktop"

    if command -v update-desktop-database >/dev/null 2>&1; then
        as_owner update-desktop-database -q "$APPLICATIONS_DIR" || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f "$ICON_ROOT/index.theme" ]; then
        as_owner gtk-update-icon-cache -q -t -f "$ICON_ROOT" || true
    fi

    echo "--- Installation Complete! ---"
    echo "Binary: $BIN_DIR/$BINARY_NAME"
    echo "Desktop entry: $APPLICATIONS_DIR/$BINARY_NAME.desktop"
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) echo "Note: $BIN_DIR is not on your PATH; the launcher entry works regardless." ;;
    esac
}

case "$(uname -s)" in
    Darwin) install_macos ;;
    Linux) install_linux ;;
    *)
        echo "Error: unsupported platform $(uname -s). Build with 'cargo build --release' and run $BUILD_DIR/$BINARY_NAME." >&2
        exit 1
        ;;
esac
