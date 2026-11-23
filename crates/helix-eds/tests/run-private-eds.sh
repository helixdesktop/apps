#!/bin/bash
# Runs a command on a PRIVATE D-Bus session with scratch EDS daemons, so tests
# never touch the user's real contacts or calendars.
#
#   crates/helix-eds/tests/run-private-eds.sh cargo test -p helix-eds --test private_bus
#
# (The script sets HELIX_EDS_PRIVATE_BUS=1 for the test.) The bus loads no
# service files (tools/testing/session.conf): the daemons are started here,
# and nothing else can be started on it.
set -u
if [ -z "${HELIX_EDS_INNER:-}" ]; then
    export HELIX_EDS_INNER=1
    conf=$(cd "$(dirname "$0")/../../../tools/testing" && pwd)/session.conf
    exec env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
        /usr/bin/dbus-run-session --config-file="$conf" -- "$0" "$@"
fi

case "${DBUS_SESSION_BUS_ADDRESS:-}" in
    ""|*"/run/user/"*"/bus"*) echo "refusing: not a private bus" >&2; exit 99;;
esac
if busctl --address="$DBUS_SESSION_BUS_ADDRESS" list --activatable --no-legend 2>/dev/null |
    awk '$1 != "org.freedesktop.DBus"' | grep -q .; then
    echo "refusing: the private bus can start real services" >&2; exit 99
fi

S=$(mktemp -d "${TMPDIR:-/tmp}/helix-eds-test.XXXXXX")
# A crashing daemon leaves a core file where the kernel's core_pattern says
# (CI points it at $EDS_CORES.*); a backtrace is printed from it below.
ulimit -c unlimited 2>/dev/null
export CARGO_HOME=${CARGO_HOME:-$HOME/.cargo} RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
export HOME=$S/home XDG_DATA_HOME=$S/data XDG_CONFIG_HOME=$S/config XDG_CACHE_HOME=$S/cache
export GSETTINGS_BACKEND=memory GIO_USE_VFS=local GVFS_DISABLE_FUSE=1 HELIX_EDS_PRIVATE_BUS=1
mkdir -p "$HOME" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"

LIBEXEC=${EDS_LIBEXEC:-/usr/libexec}
B="busctl --address=$DBUS_SESSION_BUS_ADDRESS"
"$LIBEXEC/evolution-source-registry" >"$S/registry.log" 2>&1 &
PIDS=$!
for _ in $(seq 100); do $B status org.gnome.evolution.dataserver.Sources5 >/dev/null 2>&1 && break; sleep 0.1; done
"$LIBEXEC/evolution-addressbook-factory" -r >"$S/addressbook.log" 2>&1 &
PIDS="$PIDS $!"
"$LIBEXEC/evolution-calendar-factory" -r >"$S/calendar.log" 2>&1 &
PIDS="$PIDS $!"
for _ in $(seq 100); do
    $B status org.gnome.evolution.dataserver.AddressBook10 >/dev/null 2>&1 &&
        $B status org.gnome.evolution.dataserver.Calendar8 >/dev/null 2>&1 && break
    sleep 0.1
done

"$@"
rc=$?
kill $PIDS 2>/dev/null
wait 2>/dev/null
if [ $rc -ne 0 ]; then
    # What the daemons said (a crashed or refused daemon shows up here).
    for log in "$S"/registry.log "$S"/addressbook.log "$S"/calendar.log; do
        echo "--- ${log##*/}"; tail -n 40 "$log"
    done
    # A daemon that crashed: its backtrace (needs gdb; CI sets EDS_CORES).
    if [ -n "${EDS_CORES:-}" ] && command -v gdb >/dev/null; then
        for core in "$EDS_CORES".*; do
            [ -e "$core" ] || continue
            # The kernel cuts the name to 15 characters (%e).
            name=${core#"$EDS_CORES".}; name=${name%.*}
            daemon=$(ls -d "$LIBEXEC/$name"* 2>/dev/null | head -n 1)
            echo "--- backtrace of ${daemon:-$name} ($core)"
            gdb -q -batch -ex "set debuginfod enabled on" -ex "thread apply all bt" \
                ${daemon:+"$daemon"} "$core" 2>&1 | tail -n 200
        done
    fi
fi
rm -rf "$S"
exit $rc
