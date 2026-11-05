#!/usr/bin/env bash
# Build Piya SwiftUI app (MenuBar + Hummingbird) with Rust core via UniFFI.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> Building Rust library (release, Swift — no wry/tao)…"
cargo build --release --no-default-features

LIB="$ROOT/target/release/libpiya.dylib"
if [[ ! -f "$LIB" ]]; then
  echo "Missing $LIB" >&2
  exit 1
fi

echo "==> Generating UniFFI Swift bindings…"
cargo run --release --no-default-features --bin uniffi-bindgen -- generate \
  --library \
  --language swift \
  --crate piya \
  --out-dir "$ROOT/macos/PiyaApp/Generated" \
  "$LIB"

# SwiftPM systemLibrary expects module.modulemap
cp "$ROOT/macos/PiyaApp/Generated/piyaFFI.modulemap" "$ROOT/macos/PiyaApp/Generated/module.modulemap"

echo "==> Building SwiftUI app…"
cd "$ROOT/macos/PiyaApp"
swift build -c release \
  -Xlinker -L -Xlinker "$ROOT/target/release" \
  -Xlinker -lpiya \
  -Xlinker -rpath -Xlinker @executable_path/../Frameworks

BIN="$ROOT/macos/PiyaApp/.build/release/Piya"
APP="$ROOT/dist/PiyaSwift.app"
ICON_SRC="$ROOT/assets/AppIcon.png"

echo "==> Packaging $APP …"
rm -rf "$APP"
CLI="$ROOT/target/release/piya"
if [[ ! -f "$CLI" ]]; then
  echo "Missing $CLI" >&2
  exit 1
fi

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Frameworks" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/Piya"
chmod +x "$APP/Contents/MacOS/Piya"
cp "$CLI" "$APP/Contents/Helpers/piya"
chmod +x "$APP/Contents/Helpers/piya"
cp "$LIB" "$APP/Contents/Frameworks/libpiya.dylib"

install_name_tool -id @rpath/libpiya.dylib "$APP/Contents/Frameworks/libpiya.dylib" 2>/dev/null || true
install_name_tool -add_rpath @executable_path/../Frameworks "$APP/Contents/MacOS/Piya" 2>/dev/null || true
# SwiftPM links an absolute cargo path; rewrite so the app loads the bundled dylib.
OLD_LIB="$(otool -L "$APP/Contents/MacOS/Piya" | awk '/libpiya\.dylib/{print $1; exit}')"
if [[ -n "${OLD_LIB:-}" && "$OLD_LIB" != "@rpath/libpiya.dylib" ]]; then
  install_name_tool -change "$OLD_LIB" @rpath/libpiya.dylib "$APP/Contents/MacOS/Piya"
fi

if [[ -f "$ICON_SRC" ]]; then
  cp "$ICON_SRC" "$APP/Contents/Resources/AppIcon.png"
  ICONSET="$ROOT/dist/PiyaSwift.iconset"
  rm -rf "$ICONSET"
  mkdir -p "$ICONSET"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
fi

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>Piya</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleIdentifier</key><string>com.piya.swiftui</string>
  <key>CFBundleName</key><string>Piya</string>
  <key>CFBundleDisplayName</key><string>Piya</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.2.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleIconName</key><string>AppIcon</string>
  <key>NSUserNotificationAlertStyle</key><string>alert</string>
</dict>
</plist>
EOF

codesign --force --deep --sign - "$APP" 2>/dev/null || true

echo ""
echo "Built: $APP"
echo "Run:   open \"$APP\""
echo ""
echo "Menu bar → Ask Piya"
echo "Shortcut (when Piya is focused): ⌃⌘Space"
