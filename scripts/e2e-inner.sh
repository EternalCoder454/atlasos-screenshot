#!/bin/bash
# Runs inside the AtlasOS image for scripts/e2e-kwin.sh. Not for the host.
set -uo pipefail

log=/out/e2e-$E2E_PART.log
exec > >(tee -a "$log") 2>&1

if [ "${1:-}" != --in-session ]; then
    install -m755 /in/atlasos-screenshot /usr/bin/atlasos-screenshot
    install -m644 /in/data/net.eterneon.atlas.screenshot.desktop /usr/share/applications/
    # kwin_wayland has file caps, which a rootless container can't honour;
    # a copy (keeping its name, or its QPA plugin refuses) runs without.
    mkdir -p /tmp/b && cp /usr/sbin/kwin_wayland /tmp/b/kwin_wayland
    export HOME=/tmp/h LANG=C.UTF-8 XDG_RUNTIME_DIR=/tmp/xdg
    export XDG_CONFIG_HOME=/tmp/h/.config XDG_CACHE_HOME=/tmp/h/.cache
    if [ "$E2E_PART" = online ]; then
        export XDG_DATA_HOME=/out/data
    else
        export XDG_DATA_HOME=/tmp/h/.local/share
    fi
    mkdir -p -m700 "$XDG_RUNTIME_DIR" "$HOME"
    exec dbus-run-session -- bash "$0" --in-session
fi

fails=0
check() { # name, condition result
    if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
png_size() { python3 -c 'import struct,sys; d=open(sys.argv[1],"rb").read(24); print("%dx%d" % struct.unpack(">II", d[16:24]))' "$1"; }
ms() { echo $(($(date +%s%N) / 1000000)); }
servers() { pgrep -f '^/usr/bin/atlasos-screenshot|^atlasos-screenshot' | tr '\n' ' '; }

# 1920x1200 pixels at 1.5x: a 1280x800 logical desktop, like the user's.
# (kwin's own --scale multiplies the size instead.)
/tmp/b/kwin_wayland --virtual --width 1920 --height 1200 --socket wl-test >/tmp/kwin.log 2>&1 &
kwin=$!
for _ in $(seq 60); do [ -S "$XDG_RUNTIME_DIR/wl-test" ] && break; sleep 0.25; done
export WAYLAND_DISPLAY=wl-test QT_QPA_PLATFORM=wayland
kscreen-doctor output.Virtual-0.scale.1.5 >/dev/null 2>&1
sleep 1
kdialog --title "E2E" --msgbox "Mail zach@example.com from 192.168.1.20 now" >/dev/null 2>&1 &
sleep 4

S=atlasos-screenshot
if [ "$E2E_PART" = online ]; then
    t0=$(ms)
    $S --region 0,0,1280x800; rc=$?
    t1=$(ms)
    check "image: exit 0" $rc
    echo "image: capture-to-copied ${t0:+$((t1 - t0))} ms"
    wl-paste -t image/png >/out/full.png
    check "image: clipboard has a 1920x1200 PNG" $([ "$(png_size /out/full.png)" = 1920x1200 ]; echo $?)
    pid=$(servers)
    echo "clipboard server pid(s): $pid"
    for p in $pid; do ls -l /proc/$p/fd >/out/server-fds.txt 2>&1; done
    wl-copy other
    sleep 1
    check "image: clipboard server exits when something else is copied" $([ -z "$(servers)" ]; echo $?)

    t0=$(ms)
    $S --mode text --region 0,0,1280x800; rc=$?
    t1=$(ms)
    check "text: exit 0 (first run downloads the models)" $rc
    echo "text: incl. any download $((t1 - t0)) ms"
    wl-paste -t text/plain >/out/text.txt
    echo "--- OCR text"; cat /out/text.txt; echo "---"
    check "text: OCR found the address" $(grep -qE '192\.168\.1[. ]20' /out/text.txt; echo $?)
    ls -la "$XDG_DATA_HOME/atlasos-screenshot" "$XDG_DATA_HOME/atlasos-screenshot/models"
    t0=$(ms)
    $S --mode text --region 0,0,1280x800 && t1=$(ms) && echo "text: warm $((t1 - t0)) ms"

    $S --mode redact --region 0,0,1280x800; rc=$?
    check "redact: exit 0" $rc
    wl-paste -t image/png >/out/redact.png

    # Damage a model: it must be re-downloaded, not used.
    printf 'x' | dd of="$XDG_DATA_HOME/atlasos-screenshot/models/text-detection.rten" bs=1 seek=100 conv=notrunc 2>/dev/null
    $S --mode text --region 0,0,1280x800; rc=$?
    check "text: damaged model is replaced" $rc

    # Bad config: defaults plus one warning, still works.
    mkdir -p "$XDG_CONFIG_HOME/atlasos-screenshot"
    printf '[overlay]\ndim = 7\n' >"$XDG_CONFIG_HOME/atlasos-screenshot/config.toml"
    $S --region 0,0,100x100 2>/tmp/warn.txt; rc=$?
    cat /tmp/warn.txt
    check "bad config: still copies" $rc
    check "bad config: one warning" $([ "$(grep -c overlay.dim /tmp/warn.txt)" = 1 ]; echo $?)
    rm "$XDG_CONFIG_HOME/atlasos-screenshot/config.toml"

    # The overlay: start it, then capture the screen with it up.
    $S &
    ov=$!
    sleep 2
    $S --region 0,0,1280x800; rc=$?
    wl-paste -t image/png >/out/overlay.png
    check "overlay: still running (waiting for a drag)" $(kill -0 $ov 2>/dev/null; echo $?)
    # (No notification server here: the notice itself can take up to 3 s.)
    timeout 5 $S 2>/tmp/second.txt; rc=$?
    check "overlay: a second one exits, saying one is in progress" \
        $([ $rc = 1 ] && grep -q 'already in progress' /tmp/second.txt; echo $?)
    kill $ov 2>/dev/null; wait $ov 2>/dev/null
    grep -i 'error\|warning' /tmp/kwin.log | grep -iv 'cap_sys_nice\|real time' | head -5
else
    $S --mode text --region 0,0,1280x800 2>/tmp/err.txt; rc=$?
    cat /tmp/err.txt
    check "offline text: fails" $([ $rc = 1 ]; echo $?)
    check "offline text: says no internet" $(grep -q 'no internet connection' /tmp/err.txt; echo $?)
    check "offline text: copied nothing" $([ "$(wl-paste -l 2>/dev/null)" = "" ]; echo $?)
    $S --mode redact --region 0,0,1280x800 2>/dev/null; rc=$?
    check "offline redact: fails" $([ $rc = 1 ]; echo $?)
    $S --region 0,0,1280x800; rc=$?
    check "offline image: still works" $rc
    check "offline: no model folder left with partial files" \
        $([ -z "$(find "$XDG_DATA_HOME" -name '*.tmp' 2>/dev/null)" ]; echo $?)
fi

pkill -f atlasos-screenshot
kill $kwin
echo "$E2E_PART: $fails failure(s)"
exit $fails
