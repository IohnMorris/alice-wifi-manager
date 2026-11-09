#!/bin/sh
set -eu

HOST=
USER=root
while [ "$#" -gt 0 ]; do
    case "$1" in
        --host) HOST=${2-}; shift 2 ;;
        --user) USER=${2-}; shift 2 ;;
        -h|--help) echo "Usage: $0 --host HOST [--user USER]"; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ -n "$HOST" ] || { echo "Usage: $0 --host HOST [--user USER]" >&2; exit 2; }

ssh "$USER@$HOST" 'set -eu
mount -o remount,rw / 2>/dev/null || true
if [ -x /usr/bin/alice-wifi-manager ]; then
    /usr/bin/alice-wifi-manager uninstall --yes
else
    /etc/init.d/S99z-wifi-manager stop 2>/dev/null || true
fi
'

echo "Manager removed. Recovery copy and Wi-Fi configuration were preserved."
