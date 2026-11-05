#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "Building release binary (legacy wry UI)…"
cargo build --release --features legacy-ui

APP="$ROOT/dist/Piya.app"
ICON_SRC="$ROOT/assets/AppIcon.png"
ICONSET="$ROOT/dist/Piya.iconset"

rm -rf "$ROOT/dist"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$ROOT/assets"

if [[ ! -f "$ICON_SRC" ]]; then
  echo "Generating app icon…"
  swift "$ROOT/scripts/generate-icon.swift" "$ICON_SRC"
fi

echo "Building .icns…"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cp target/release/piya "$APP/Contents/MacOS/piya"
chmod +x "$APP/Contents/MacOS/piya"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>piya</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleIdentifier</key><string>com.piya.app</string>
  <key>CFBundleName</key><string>Piya</string>
  <key>CFBundleDisplayName</key><string>Piya</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

echo ""
echo "Signing app (ad-hoc — required for Notification Center)…"
codesign --force --deep --sign - "$APP"

echo ""
echo "Built: $APP"
echo "Install:  cp -R \"$APP\" /Applications/ && open -a \"$APP\""
echo "Or run:   ./target/release/piya app --install"
