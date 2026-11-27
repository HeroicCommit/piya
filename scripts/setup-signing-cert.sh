#!/usr/bin/env bash
# Creates a persistent self-signed "Piya Local Code Signing" certificate in your
# macOS login Keychain so Screen Recording & Accessibility permissions survive
# every future rebuild and `piya update --local` without prompting again!
#
# (For public GitHub Releases distributed to other Macs, use an Apple
#  "Developer ID Application" certificate instead — build-swift-app.sh
#  auto-detects Developer ID first, then "Piya Local Code Signing".)
set -euo pipefail

CERT_NAME="Piya Local Code Signing"
KEYCHAIN="${HOME}/Library/Keychains/login.keychain-db"
if [[ ! -f "$KEYCHAIN" ]]; then
  KEYCHAIN="${HOME}/Library/Keychains/login.keychain"
fi

if security find-identity -v -p codesigning "$KEYCHAIN" 2>/dev/null | grep -Fq "\"$CERT_NAME\""; then
  echo "✔ '$CERT_NAME' is already installed and trusted for code signing in $KEYCHAIN."
  exit 0
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

cat > "$TMP_DIR/openssl.cnf" <<EOF
[ req ]
distinguished_name = req_dn
x509_extensions    = codesign_ext
prompt             = no

[ req_dn ]
CN = ${CERT_NAME}
O  = Piya Local Development

[ codesign_ext ]
keyUsage         = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
basicConstraints = critical, CA:false
EOF

echo "==> Generating 10-year local code-signing certificate ('$CERT_NAME')…"
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "$TMP_DIR/key.pem" \
  -out "$TMP_DIR/cert.pem" \
  -days 3650 \
  -config "$TMP_DIR/openssl.cnf" >/dev/null 2>&1

# Legacy-compatible PKCS#12 so macOS `security import` accepts it across OpenSSL 3.x + LibreSSL.
openssl pkcs12 -export -legacy \
  -inkey "$TMP_DIR/key.pem" \
  -in "$TMP_DIR/cert.pem" \
  -name "$CERT_NAME" \
  -passout pass:piya-local \
  -out "$TMP_DIR/cert.p12" 2>/dev/null \
|| openssl pkcs12 -export \
  -inkey "$TMP_DIR/key.pem" \
  -in "$TMP_DIR/cert.pem" \
  -name "$CERT_NAME" \
  -passout pass:piya-local \
  -out "$TMP_DIR/cert.p12"

echo "==> Importing into $KEYCHAIN (macOS may prompt for your login password to trust code signing)…"
security import "$TMP_DIR/cert.p12" \
  -k "$KEYCHAIN" \
  -P "piya-local" \
  -T /usr/bin/codesign \
  -T /usr/bin/security

security add-trusted-cert -d -r trustRoot -p codeSign -k "$KEYCHAIN" "$TMP_DIR/cert.pem"

echo ""
if security find-identity -v -p codesigning "$KEYCHAIN" | grep -Fq "\"$CERT_NAME\""; then
  echo "✔ '$CERT_NAME' ready!"
  echo ""
  echo "Next steps (one time only):"
  echo "  1. Rebuild & install with the new stable certificate:"
  echo "       ./scripts/build-swift-app.sh && ./target/release/piya update --local"
  echo "  2. Grant Screen Recording & Accessibility ONCE in Piya → Settings → Activity."
  echo "  3. Every future build/update will keep permissions automatically without asking again."
else
  echo "Certificate imported into Keychain, but not yet marked trusted for Code Signing." >&2
  echo "Open Keychain Access → login → '$CERT_NAME' → Get Info → Trust → Code Signing: Always Trust." >&2
  exit 1
fi
