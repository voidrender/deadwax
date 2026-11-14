#!/usr/bin/env bash
# Cargo runner on macOS (see .cargo/config.toml). Signs the deadwax binary
# with a stable identity before running it, so the keychain keeps trusting
# it across rebuilds. Other binaries, such as test executables, run as is.
#
# DEADWAX_SIGN_IDENTITY picks the identity (default "Deadwax Dev"). When
# it is not in the keychain, the binary runs with its ad-hoc signature.
set -euo pipefail

bin="$1"
identity="${DEADWAX_SIGN_IDENTITY:-Deadwax Dev}"

if [[ "$(basename "$bin")" == "deadwax" ]]; then
  if security find-identity -p codesigning 2>/dev/null | grep -qF "\"$identity\""; then
    codesign --force --sign "$identity" --identifier deadwax "$bin" 2>/dev/null ||
      echo "warning: couldn't sign $bin with \"$identity\"" >&2
  elif [[ -z "${DEADWAX_SIGN_QUIET:-}" ]]; then
    echo "note: no \"$identity\" signing identity, so macOS may ask for keychain" \
      "access after each rebuild. Run scripts/make-dev-cert.sh to fix that," \
      "or set DEADWAX_SIGN_QUIET=1 to hide this note." >&2
  fi
fi

exec "$@"
