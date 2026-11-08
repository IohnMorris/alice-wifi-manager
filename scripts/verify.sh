#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=${TARGET:-armv7-unknown-linux-musleabihf}
BIN="$ROOT/target/$TARGET/release/alice-wifi-manager"

test -s "$BIN" || {
    echo "Missing release binary: $BIN" >&2
    exit 1
}

command -v file >/dev/null 2>&1 && file "$BIN"
command -v sha256sum >/dev/null 2>&1 && sha256sum "$BIN"
test -s "$ROOT/web/index.html"
test -s "$ROOT/scripts/S99z-wifi-manager"

echo "Source and local release checks passed."

