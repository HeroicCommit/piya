#!/usr/bin/env bash
# Build, package, checksum, tag, and publish a Piya macOS release WITHOUT
# modifying or reinstalling the local /Applications/Piya.app.
#
# Usage:
#   ./scripts/release.sh <version> [--package-only] [--dry-run]
#
# Examples:
#   ./scripts/release.sh 0.2.1 --package-only   # Build zip + SHA256SUMS locally only
#   ./scripts/release.sh 0.2.1                  # Bump Cargo.toml, tag v0.2.1, push & create GitHub Release
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <version> [--package-only] [--dry-run]" >&2
  exit 1
fi

RAW_VER="$1"
shift
VERSION="${RAW_VER#v}"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid semantic version: '$VERSION' (expected e.g. 0.2.1)" >&2
  exit 1
fi

PACKAGE_ONLY=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --package-only) PACKAGE_ONLY=1 ;;
    --dry-run) DRY_RUN=1 ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 1
      ;;
  esac
done

if [[ "$PACKAGE_ONLY" -eq 0 && "$DRY_RUN" -eq 0 ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "Working tree has uncommitted changes. Commit or stash first (or pass --package-only)." >&2
    exit 1
  fi
  if ! command -v gh >/dev/null 2>&1; then
    echo "GitHub CLI (gh) is required to publish a release (or pass --package-only)." >&2
    exit 1
  fi
fi

CURRENT_VERSION="$(awk -F'"' '/^version = "/{print $2; exit}' "$ROOT/Cargo.toml")"
if [[ "$CURRENT_VERSION" != "$VERSION" ]]; then
  echo "==> Updating Cargo.toml version: ${CURRENT_VERSION} -> ${VERSION}"
  if [[ "$DRY_RUN" -eq 0 ]]; then
    python3 - "$ROOT/Cargo.toml" "$VERSION" <<'PY'
import pathlib, re, sys
p = pathlib.Path(sys.argv[1])
ver = sys.argv[2]
src = p.read_text()
updated, n = re.subn(r'(?m)^version = "[^"]+"', f'version = "{ver}"', src, count=1)
assert n == 1, "Failed to locate package version in Cargo.toml"
p.write_text(updated)
PY
  fi
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "[dry-run] Would test, build Piya v${VERSION}, package Piya-macos-arm64.zip + SHA256SUMS, tag v${VERSION}, and publish GitHub Release."
  exit 0
fi

echo "==> Running unit tests…"
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --no-default-features

echo "==> Building isolated release bundle (leaves /Applications/Piya.app untouched)…"
"$ROOT/scripts/build-swift-app.sh"

REL_DIR="$ROOT/dist/release.noindex"
rm -rf "$REL_DIR"
mkdir -p "$REL_DIR"

# Rename inside archive to canonical `Piya.app` so extracted bundle is ready to drop in /Applications.
ditto "$ROOT/dist/build.noindex/PiyaSwift.app" "$REL_DIR/Piya.app"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u "$REL_DIR/Piya.app" 2>/dev/null || true

ARCH="$(uname -m)"
if [[ "$ARCH" == "arm64" ]]; then
  ASSET_NAME="Piya-macos-arm64.zip"
else
  ASSET_NAME="Piya-macos-${ARCH}.zip"
fi

echo "==> Creating ${ASSET_NAME} and SHA256SUMS…"
(
  cd "$REL_DIR"
  ditto -c -k --sequesterRsrc --keepParent "Piya.app" "$ASSET_NAME"
  shasum -a 256 "$ASSET_NAME" > SHA256SUMS
  rm -rf "Piya.app"
)

echo "Packaged:"
echo "  $REL_DIR/$ASSET_NAME"
echo "  $REL_DIR/SHA256SUMS ($(cat "$REL_DIR/SHA256SUMS"))"

if [[ "$PACKAGE_ONLY" -eq 1 ]]; then
  echo ""
  echo "Done (--package-only). Installed /Applications/Piya.app was not modified."
  exit 0
fi

TAG="v${VERSION}"
if [[ -n "$(git status --porcelain Cargo.toml Cargo.lock 2>/dev/null)" ]]; then
  git add Cargo.toml Cargo.lock
  git commit -m "Release ${TAG}"
fi

git tag -a "$TAG" -m "Piya ${TAG}"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
git push origin "$BRANCH"
git push origin "$TAG"

echo "==> Publishing GitHub Release ${TAG}…"
gh release create "$TAG" \
  "$REL_DIR/$ASSET_NAME" \
  "$REL_DIR/SHA256SUMS" \
  --title "Piya ${TAG}" \
  --generate-notes

echo ""
echo "Published ${TAG}! Installed Piya instances can now update in place via:"
echo "  • Menu bar → Check for Updates… → Update & Relaunch"
echo "  • piya update"
