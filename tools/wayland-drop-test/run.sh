#!/usr/bin/env bash
# End-to-end test of dropping a file onto canopy on native Wayland.
#
# Starts a headless KWin (kwin_wayland --virtual) on a private D-Bus session,
# runs canopy in it, and drags a WAV onto canopy's window with KWin's fake
# input. Passes if canopy imports the file. Your own session, settings and
# recent files are left alone: everything runs with temporary XDG dirs.
#
# Needs kwin_wayland and dbus-run-session. Run from anywhere:
#   tools/wayland-drop-test/run.sh
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

if [[ "${1:-}" != "--inside" ]]; then
    command -v kwin_wayland >/dev/null || { echo "needs kwin_wayland"; exit 2; }
    cargo build --quiet --manifest-path "$root/Cargo.toml" -p ui
    cargo build --quiet --manifest-path "$here/Cargo.toml" --target-dir "$root/target/wayland-drop-test"
    # A session bus that can't start services: otherwise KWin's requests for
    # portals and the like start them, and they outlive the test, attached to
    # your real session.
    bus_config="$(mktemp)"
    cat >"$bus_config" <<'XML'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
XML
    status=0
    timeout -k 5 120 dbus-run-session --config-file="$bus_config" -- "$0" --inside || status=$?
    rm -f "$bus_config"
    exit "$status"
fi

work="$(mktemp -d)"
pids=()
cleanup() {
    kill "${pids[@]}" 2>/dev/null
    wait 2>/dev/null
    rm -rf "$work"
}
trap cleanup EXIT
export XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data" XDG_CACHE_HOME="$work/cache"
wav="$work/drop me.wav"
python3 - "$wav" <<'PY'
import sys, struct, math
rate = 44100
data = b"".join(struct.pack("<h", int(math.sin(i * 220 * 2 * math.pi / rate) * 20000)) for i in range(rate))
fmt = struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16)
open(sys.argv[1], "wb").write(b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt " + fmt + b"data" + struct.pack("<I", len(data)) + data)
PY

socket="canopy-drop-test-$$"
KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 kwin_wayland --virtual --no-lockscreen \
    --socket "$socket" --width 1600 --height 1000 >"$work/kwin.log" 2>&1 &
pids+=($!)
for _ in $(seq 100); do [[ -S "$XDG_RUNTIME_DIR/$socket" ]] && break; sleep 0.1; done

env -u DISPLAY WAYLAND_DISPLAY="$socket" CANOPY_DEBUG_DROPS=1 \
    "$root/target/debug/canopy" >"$work/canopy.log" 2>&1 &
pids+=($!)
sleep 6
# KWin centers windows: canopy is around the middle, the drag source lands
# on top of it in the middle, and (400, 450) is on canopy, left of it.
env -u DISPLAY WAYLAND_DISPLAY="$socket" \
    "$root/target/wayland-drop-test/debug/wayland-drop-test" "$wav" 1600 1000 400 450 \
    >"$work/source.log" 2>&1 || true
for _ in $(seq 50); do grep -q "Imported drop me.wav" "$work/canopy.log" && break; sleep 0.1; done

if grep -q "Imported drop me.wav" "$work/canopy.log"; then
    echo "PASS: canopy imported the dropped file"
else
    echo "FAIL: no import. Drag source said:"; cat "$work/source.log"
    echo "canopy said:"; grep "canopy:" "$work/canopy.log" | head -20
    exit 1
fi
