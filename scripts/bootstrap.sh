#!/bin/sh
set -eu

REPO="IohnMorris/alice-wifi-manager"
ARCHIVE="alice-wifi-manager-armv7.tar.gz"
BASE_URL="https://github.com/$REPO/releases/latest/download"

usage() {
    echo "Usage: $0 --host HOST [--user USER]"
}

HOST=
USER=root
while [ "$#" -gt 0 ]; do
    case "$1" in
        --host) HOST=${2-}; shift 2 ;;
        --user) USER=${2-}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

[ -n "$HOST" ] || { usage >&2; exit 2; }
command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "tar is required" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || {
    echo "sha256sum is required" >&2
    exit 1
}

WORK=$(mktemp -d "${TMPDIR:-/tmp}/alice-wifi-manager.XXXXXX")
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT HUP INT TERM

if ! curl -fL --retry 3 "$BASE_URL/$ARCHIVE" -o "$WORK/$ARCHIVE"; then
    TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": "\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$TAG" ] || { echo "Unable to determine latest release tag" >&2; exit 1; }
    ARCHIVE="alice-wifi-manager-${TAG}-armv7.tar.gz"
    curl -fL --retry 3 "$BASE_URL/$ARCHIVE" -o "$WORK/$ARCHIVE"
fi
curl -fL --retry 3 "$BASE_URL/SHA256SUMS" -o "$WORK/SHA256SUMS"
grep "  $ARCHIVE\$" "$WORK/SHA256SUMS" | (
    cd "$WORK"
    sha256sum -c -
)

mkdir "$WORK/package"
tar -xzf "$WORK/$ARCHIVE" -C "$WORK/package"
PACKAGE_DIR="$WORK/package/alice-wifi-manager"
exec "$PACKAGE_DIR/scripts/install.sh" --host "$HOST" --user "$USER" \
    --package "$PACKAGE_DIR"
