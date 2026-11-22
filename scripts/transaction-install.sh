#!/bin/sh
# BusyBox-compatible transactional installer. Arguments: package-dir stamp
set -u

PACKAGE=${1-}
STAMP=${2-}
LOCK=/var/run/alice-wifi-manager.install.lock
RECOVERY_ROOT=/bkupgrade/data
RECOVERY="$RECOVERY_ROOT/recovery-$STAMP"
MANAGER=/usr/bin/alice-wifi-manager
WPA=/usr/sbin/wpa_supplicant
INIT=/etc/init.d/S99z-wifi-manager
SUCCESS=0
REPLACED=0
ROLLING_BACK=0

fail() {
    echo "transaction install: $*" >&2
    return 1
}

restore_one() {
    source=$1
    target=$2
    marker="$RECOVERY/$(basename "$target").missing"
    if [ -f "$marker" ]; then
        rm -f "$target"
    elif [ -s "$source" ]; then
        temp="$target.alice-restore.$$"
        cp "$source" "$temp" || return 1
        chmod 0755 "$temp" || return 1
        mv -f "$temp" "$target" || return 1
    else
        return 1
    fi
}

rollback() {
    [ "$ROLLING_BACK" -eq 0 ] || return 1
    ROLLING_BACK=1
    "$INIT" stop >/dev/null 2>&1 || true
    restore_one "$RECOVERY/alice-wifi-manager" "$MANAGER" || return 1
    restore_one "$RECOVERY/wpa_supplicant" "$WPA" || return 1
    restore_one "$RECOVERY/S99z-wifi-manager" "$INIT" || return 1
    [ -x "$INIT" ] && "$INIT" start >/dev/null 2>&1 || true
    return 0
}

prune_recovery() {
    keep=0
    for directory in $(ls -dt "$RECOVERY_ROOT"/recovery-* 2>/dev/null); do
        keep=$((keep + 1))
        if [ "$keep" -gt 3 ] && [ "$directory" != "$RECOVERY" ]; then
            rm -rf "$directory" 2>/dev/null || true
        fi
    done
}

cleanup() {
    rm -rf "$PACKAGE" 2>/dev/null || true
    rmdir "$LOCK" 2>/dev/null || true
}

on_exit() {
    rc=$?
    if [ "$rc" -ne 0 ] && [ "$SUCCESS" -eq 0 ] && [ "$REPLACED" -eq 1 ]; then
        if ! rollback; then
            echo "transaction install: rollback failed; recovery kept at $RECOVERY" >&2
        fi
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT HUP INT TERM

[ -n "$PACKAGE" ] || exit 2
[ -n "$STAMP" ] || exit 2
[ -d "$PACKAGE" ] || { fail "package directory is missing"; exit 1; }
[ -s "$PACKAGE/alice-wifi-manager" ] || { fail "manager package is missing"; exit 1; }
[ -s "$PACKAGE/wpa_supplicant" ] || { fail "wpa_supplicant package is missing"; exit 1; }
[ -s "$PACKAGE/S99z-wifi-manager" ] || { fail "init package is missing"; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { fail "sha256sum is required on the device"; exit 1; }
EXPECTED_WPA_SHA256=3e7a5dd461d24fa72e8cc9ab53f1a2274f2df61a14a5ffe8cdb8b0f8d151f3bc
ACTUAL_WPA_SHA256=$(sha256sum "$PACKAGE/wpa_supplicant" | awk '{print $1}')
[ "$ACTUAL_WPA_SHA256" = "$EXPECTED_WPA_SHA256" ] || { fail "wpa_supplicant checksum mismatch"; exit 1; }
mkdir "$LOCK" 2>/dev/null || { fail "another installation is already running"; exit 1; }
printf '%s\n' "$$" > "$LOCK/pid"
mkdir -p "$RECOVERY_ROOT" "$RECOVERY" || { fail "cannot create recovery directory"; exit 1; }

# Ensure the writable remount really worked; silently continuing would make a partial update likely.
mount -o remount,rw / >/dev/null 2>&1 || true
test -w /usr/bin -a -w /usr/sbin -a -w /etc/init.d || {
    fail "target filesystems are not writable"
    exit 1
}

backup_one() {
    source=$1
    name=$2
    if [ -e "$source" ]; then
        cp "$source" "$RECOVERY/$name" || return 1
        test -s "$RECOVERY/$name" || return 1
    else
        : > "$RECOVERY/$name.missing" || return 1
    fi
}
backup_one "$MANAGER" alice-wifi-manager || { fail "manager backup failed"; exit 1; }
backup_one "$WPA" wpa_supplicant || { fail "wpa_supplicant backup failed"; exit 1; }
backup_one "$INIT" S99z-wifi-manager || { fail "init backup failed"; exit 1; }
backup_one /bkupgrade/data/wpa_supplicant.conf wpa_supplicant.conf || { fail "Wi-Fi config backup failed"; exit 1; }
backup_one /bkupgrade/data/wpa_supplicant.conf.bak wpa_supplicant.conf.bak || { fail "Wi-Fi config backup failed"; exit 1; }
backup_one /bkupgrade/data/wpa_supplicant.ap.conf wpa_supplicant.ap.conf || { fail "AP config backup failed"; exit 1; }
: > "$RECOVERY/MANIFEST.sha256" || { fail "recovery manifest failed"; exit 1; }
for saved in alice-wifi-manager wpa_supplicant S99z-wifi-manager wpa_supplicant.conf wpa_supplicant.conf.bak wpa_supplicant.ap.conf; do
    if [ -f "$RECOVERY/$saved" ]; then
        sha256sum "$RECOVERY/$saved" >> "$RECOVERY/MANIFEST.sha256" || { fail "recovery manifest failed"; exit 1; }
    fi
done

# Normalize and stage on the destination filesystem before touching the running service.
sed 's/\r$//' "$PACKAGE/S99z-wifi-manager" > "$PACKAGE/S99z-wifi-manager.normalized" || exit 1
install -m 0755 "$PACKAGE/alice-wifi-manager" "$MANAGER.alice-new.$$" || exit 1
install -m 0755 "$PACKAGE/wpa_supplicant" "$WPA.alice-new.$$" || exit 1
install -m 0755 "$PACKAGE/S99z-wifi-manager.normalized" "$INIT.alice-new.$$" || exit 1

[ ! -x "$INIT" ] || "$INIT" stop || { fail "existing manager did not stop"; exit 1; }
[ "$(pidof alice-wifi-manager 2>/dev/null | wc -w)" -eq 0 ] || {
    fail "manager process is still running"
    exit 1
}
mv -f "$MANAGER.alice-new.$$" "$MANAGER" || exit 1
REPLACED=1
mv -f "$WPA.alice-new.$$" "$WPA" || exit 1
mv -f "$INIT.alice-new.$$" "$INIT" || exit 1

"$INIT" start || { fail "new manager failed to start"; exit 1; }
sleep 1
[ "$(pidof alice-wifi-manager 2>/dev/null | wc -w)" -eq 1 ] || { fail "manager PID health check failed"; exit 1; }
test -S /var/run/alice-wifi-manager.sock || { fail "manager control socket is missing"; exit 1; }
"$MANAGER" status >/dev/null || { fail "manager status health check failed"; exit 1; }
[ "$(pidof wpa_supplicant 2>/dev/null | wc -w)" -le 1 ] || { fail "multiple wpa_supplicant processes detected"; exit 1; }

SUCCESS=1
rm -f "$MANAGER.alice-new.$$" "$WPA.alice-new.$$" "$INIT.alice-new.$$"
prune_recovery
echo "transaction install: success; recovery=$RECOVERY"
exit 0
