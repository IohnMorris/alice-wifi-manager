#!/bin/sh
set -eu

usage() {
    echo "Usage: $0 --host HOST [--user USER] [--package DIR]"
}

HOST=
USER=root
PACKAGE_DIR=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --host) HOST=${2-}; shift 2 ;;
        --user) USER=${2-}; shift 2 ;;
        --package) PACKAGE_DIR=${2-}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

[ -n "$HOST" ] || { usage >&2; exit 2; }
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PACKAGE_DIR=${PACKAGE_DIR:-"$ROOT"}
MANAGER="$PACKAGE_DIR/alice-wifi-manager"
WPA="$PACKAGE_DIR/wpa_supplicant"
INIT="$PACKAGE_DIR/S99z-wifi-manager"

for file in "$MANAGER" "$WPA" "$INIT"; do
    test -s "$file" || { echo "Missing package file: $file" >&2; exit 1; }
done

STAMP=$(date +%Y%m%d-%H%M%S)-$$
REMOTE="/tmp/alice-wifi-manager-$STAMP"
TRANSACTION="$PACKAGE_DIR/transaction-install.sh"
[ -s "$TRANSACTION" ] || TRANSACTION="$PACKAGE_DIR/scripts/transaction-install.sh"
test -s "$TRANSACTION" || { echo "Missing transaction installer" >&2; exit 1; }
ssh "$USER@$HOST" "umask 077; mkdir '$REMOTE'"
scp "$MANAGER" "$WPA" "$INIT" "$TRANSACTION" "$USER@$HOST:$REMOTE/"
ssh "$USER@$HOST" "sh '$REMOTE/transaction-install.sh' '$REMOTE' '$STAMP'"
echo "Installation completed. Recovery copy: /bkupgrade/data/recovery-$STAMP"
