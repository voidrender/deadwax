#!/usr/bin/env bash
# Creates a self-signed code signing identity for development builds.
#
# macOS ties keychain access to an app's signature. Cargo signs each build
# with a fresh ad-hoc signature, so every rebuild looks like a new app and
# the keychain asks for your password again. Builds signed with one stable
# identity keep their access across rebuilds.
#
# Usage: scripts/make-dev-cert.sh [name]   (default name: "Deadwax Dev")
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: dev signing is only needed on macOS" >&2
  exit 1
fi

name="${1:-Deadwax Dev}"
keychain="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$name" "$keychain" >/dev/null 2>&1; then
  echo "A certificate named \"$name\" is already in your login keychain."
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat >"$work/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF

openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -config "$work/cert.cnf" -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null

# The keychain only reads PKCS#12 files with the older ciphers, and the file
# never leaves the temporary directory.
pass="$(openssl rand -hex 16)"
legacy=()
if openssl pkcs12 -help 2>&1 | grep -q -- '-legacy'; then
  legacy=(-legacy)
fi
openssl pkcs12 -export "${legacy[@]}" -name "$name" \
  -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/identity.p12" -passout "pass:$pass"

# -T lets codesign use the key without asking each time.
security import "$work/identity.p12" -k "$keychain" -P "$pass" -T /usr/bin/codesign

# Newer macOS also checks the key's partition list. Adding codesign to it
# here stops the "Always Allow" dialog on the first signing. The security
# command asks for your login password in the terminal.
echo "Enter your login password to let codesign use the new key."
security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -t private -l "$name" "$keychain" >/dev/null

echo "Created \"$name\". cargo run now signs Deadwax with it."
