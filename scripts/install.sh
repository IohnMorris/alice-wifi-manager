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

STAMP=$(date +%Y%m%d-%H%M%S)
REMOTE="/tmp/alice-wifi-manager-$STAMP"
ssh "$USER@$HOST" "mkdir -p '$REMOTE' /bkupgrade/data/recovery-$STAMP"
scp "$MANAGER" "$WPA" "$INIT" "$USER@$HOST:$REMOTE/"

ssh "$USER@$HOST" "set -eu
mount -o remount,rw / 2>/dev/null || true
cp /usr/bin/alice-wifi-manager /bkupgrade/data/recovery-$STAMP/alice-wifi-manager 2>/dev/null || true
cp /usr/sbin/wpa_supplicant /bkupgrade/data/recovery-$STAMP/wpa_supplicant 2>/dev/null || true
cp /etc/init.d/S99z-wifi-manager /bkupgrade/data/recovery-$STAMP/S99z-wifi-manager 2>/dev/null || true
cp /bkupgrade/data/wpa_supplicant.conf /bkupgrade/data/recovery-$STAMP/wpa_supplicant.conf 2>/dev/null || true
cp /bkupgrade/data/wpa_supplicant.conf.bak /bkupgrade/data/recovery-$STAMP/wpa_supplicant.conf.bak 2>/dev/null || true
cp /bkupgrade/data/wpa_supplicant.ap.conf /bkupgrade/data/recovery-$STAMP/wpa_supplicant.ap.conf 2>/dev/null || true
/etc/init.d/S99z-wifi-manager stop 2>/dev/null || true
install -m 0755 '$REMOTE/alice-wifi-manager' /usr/bin/alice-wifi-manager
install -m 0755 '$REMOTE/wpa_supplicant' /usr/sbin/wpa_supplicant
install -m 0755 '$REMOTE/S99z-wifi-manager' /etc/init.d/S99z-wifi-manager
sed -i 's/\r$//' /etc/init.d/S99z-wifi-manager
/etc/init.d/S99z-wifi-manager start
test \$(pidof alice-wifi-manager | wc -w) -eq 1
/usr/bin/alice-wifi-manager status
rm -rf '$REMOTE'
"

echo "Installation completed. Recovery copy: /bkupgrade/data/recovery-$STAMP"
