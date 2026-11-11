#!/usr/bin/env bash
# Build Piya SwiftUI app (MenuBar + Hummingbird) with Rust core via UniFFI.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DEV_MODE=0
for arg in "$@"; do
  case "$arg" in
    --dev) DEV_MODE=1 ;;
    -h|--help)
      echo "Usage: $0 [--dev]"
      echo "  --dev   Build isolated side-by-side 'Piya Dev' bundle (com.piya.swiftui.dev)"
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      exit 1
      ;;
  esac
done

VERSION="$(awk -F'"' '/^version = "/{print $2; exit}' "$ROOT/Cargo.toml")"
if [[ -z "$VERSION" ]]; then
  echo "Failed to read version from Cargo.toml" >&2
  exit 1
fi

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
swift_build() {
  swift build -c release \
    -Xlinker -L -Xlinker "$ROOT/target/release" \
    -Xlinker -lpiya \
    -Xlinker -rpath -Xlinker @executable_path/../Frameworks
}
if ! swift_build; then
  # Some Command Line Tools releases ship an SDK newer than their Swift compiler
  # and fail on SwiftUI property wrappers (@State). Retry once with the newest
  # older SDK. Override with SDKROOT=/path/to/MacOSX*.sdk.
  if [[ -z "${SDKROOT:-}" ]]; then
    FALLBACK_SDK="$(ls -d /Library/Developer/CommandLineTools/SDKs/MacOSX[0-9]*.[0-9]*.sdk 2>/dev/null | sort -V | tail -2 | head -1 || true)"
    if [[ -n "$FALLBACK_SDK" ]]; then
      echo "==> Swift build failed; retrying with SDK $FALLBACK_SDK"
      SDKROOT="$FALLBACK_SDK" swift_build
    else
      exit 1
    fi
  else
    exit 1
  fi
fi

BIN="$ROOT/macos/PiyaApp/.build/release/Piya"
STAGE="$ROOT/dist/build.noindex"
if [[ "$DEV_MODE" -eq 1 ]]; then
  APP="$STAGE/PiyaDev.app"
  BUNDLE_ID="com.piya.swiftui.dev"
  BUNDLE_NAME="Piya Dev"
  SHORT_VERSION="${VERSION}-dev"
else
  APP="$STAGE/PiyaSwift.app"
  BUNDLE_ID="com.piya.swiftui"
  BUNDLE_NAME="Piya"
  SHORT_VERSION="$VERSION"
fi
ICON_SRC="$ROOT/assets/AppIcon.png"

echo "==> Packaging $APP …"
rm -rf "$APP"
CLI="$ROOT/target/release/piya"
if [[ ! -f "$CLI" ]]; then
  echo "Missing $CLI" >&2
  exit 1
fi

# `.noindex` suffix guarantees Spotlight / Raycast skips staged build bundles.
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
  ICONSET="$STAGE/PiyaSwift.iconset"
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
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleName</key><string>${BUNDLE_NAME}</string>
  <key>CFBundleDisplayName</key><string>${BUNDLE_NAME}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${SHORT_VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleIconName</key><string>AppIcon</string>
  <key>NSUserNotificationAlertStyle</key><string>alert</string>
</dict>
</plist>
EOF

SIGN_IDENTITY="${PIYA_CODESIGN_IDENTITY:--}"
codesign --force --deep --sign "$SIGN_IDENTITY" "$APP" 2>/dev/null || true
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u "$APP" 2>/dev/null || true

echo ""
echo "Built:          $APP (v${SHORT_VERSION})"
if [[ "$DEV_MODE" -eq 1 ]]; then
  echo "Run side-by-side (leaves /Applications/Piya.app untouched):"
  echo "  open \"$APP\""
else
  echo "Update in place: ./target/release/piya update --local"
  echo "First install:   ./target/release/piya app --install"
fi
echo ""
echo "Menu bar → Ask Piya"
echo "Shortcut (when Piya is focused): ⌃⌘Space"
