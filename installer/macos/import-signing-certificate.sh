#!/usr/bin/env bash
# For CI only: imports a Developer ID Application certificate into a
# temporary keychain so codesign can use it. Quintess has no Apple Developer
# account yet, so today this finds no certificate and leaves the build
# ad-hoc signed; it is here so real signing only needs the secrets added.
#
# Inputs (environment): APPLE_CERTIFICATE (the .p12, base64-encoded),
# APPLE_CERTIFICATE_PASSWORD, KEYCHAIN_PASSWORD (optional; random if unset),
# RUNNER_TEMP (or TMPDIR).
# Outputs: MACOS_SIGNING_IDENTITY (the identity's SHA-1) and MACOS_KEYCHAIN,
# appended to $GITHUB_ENV in GitHub Actions, otherwise printed.
#
# Adapted from PdfCraft's packaging/macos/import-cert.sh
# (https://github.com/storytold/pdfcraft, commit 6d843a8e8b22e4442666eb9e12f506c40458af3d),
# Copyright (c) 2026 ArtCraft Team and the PdfCraft contributors, MIT OR Apache-2.0.
# Modified: Quintess's names and variable names, no shared env.sh.
set -euo pipefail
umask 077

if [ -z "${APPLE_CERTIFICATE:-}" ]; then
  echo "No APPLE_CERTIFICATE: the app is ad-hoc signed and not notarized."
  exit 0
fi

temporary="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
keychain="$temporary/quintess-signing.keychain-db"
certificate="$temporary/quintess-signing.p12"
keychain_password="${KEYCHAIN_PASSWORD:-$(openssl rand -hex 24)}"

trap 'rm -f "$certificate"' EXIT
printf '%s' "$APPLE_CERTIFICATE" | base64 --decode >"$certificate"
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$certificate" -P "${APPLE_CERTIFICATE_PASSWORD:-}" -T /usr/bin/codesign -T /usr/bin/security -t cert -f pkcs12 -k "$keychain"
rm -f "$certificate"
# Lets codesign use the key without asking.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" >/dev/null
# On the search list, keeping the login keychain, so codesign and notarytool find it.
# shellcheck disable=SC2046
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')

identity="$(security find-identity -v -p codesigning "$keychain" | awk '/Developer ID Application/ { print $2; exit }')"
if [ -z "$identity" ]; then
  security find-identity -v -p codesigning "$keychain" >&2
  echo "error: APPLE_CERTIFICATE holds no 'Developer ID Application' identity" >&2
  exit 1
fi
echo "Imported signing identity $identity"
if [ -n "${GITHUB_ENV:-}" ]; then
  { echo "MACOS_SIGNING_IDENTITY=$identity"; echo "MACOS_KEYCHAIN=$keychain"; } >>"$GITHUB_ENV"
else
  echo "MACOS_SIGNING_IDENTITY=$identity"
  echo "MACOS_KEYCHAIN=$keychain"
fi
