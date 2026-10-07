#!/bin/bash
# Runs inside the Telamon OS image for scripts/e2e-kwin.sh. Not for the host.
set -uo pipefail

log=/out/e2e-$E2E_PART.log
exec > >(tee -a "$log") 2>&1

if [ "${1:-}" != --in-session ]; then
    install -m755 /in/telamon-screenshot /usr/bin/telamon-screenshot
    # The old name, as the package ships it for this release.
    ln -s telamon-screenshot /usr/bin/atlasos-screenshot
    install -m644 /in/data/net.eterneon.telamon.screenshot.desktop /usr/share/applications/
    # kwin_wayland has file caps, which a rootless container can't honour;
    # a copy (keeping its name, or its QPA plugin refuses) runs without.
    mkdir -p /tmp/b && cp /usr/sbin/kwin_wayland /tmp/b/kwin_wayland
    export HOME=/tmp/h LANG=C.UTF-8 XDG_RUNTIME_DIR=/tmp/xdg
    export XDG_CONFIG_HOME=/tmp/h/.config XDG_CACHE_HOME=/tmp/h/.cache
    if [ "$E2E_PART" = online ] || [ "$E2E_PART" = modes ]; then
        export XDG_DATA_HOME=/out/data
    else
        export XDG_DATA_HOME=/tmp/h/.local/share
    fi
    mkdir -p -m700 "$XDG_RUNTIME_DIR" "$HOME"
    if [ "$E2E_PART" = modes ]; then
        # As the image will be once Spectacle is gone: our org.kde.Spectacle
        # service answers instead of /usr/bin/spectacle's.
        install -m644 /in/data/org.kde.Spectacle.service /usr/share/dbus-1/services/org.kde.Spectacle.service
        rm -f /usr/share/dbus-1/services/org.kde.spectacle.service
        # Stand-ins for what the notification buttons start.
        mkdir -p /usr/local/bin
        install -m755 /dev/stdin /usr/local/bin/xdg-open <<'STUB'
#!/bin/sh
echo "$*" >>/out/xdg-open.log
STUB
        install -m755 /dev/stdin /usr/bin/telamon-screenshot-editor <<'STUB'
#!/bin/sh
echo "$*" >>/out/editor.log
if [ "$1" = "-" ]; then cat >/out/editor-stdin.png; fi
STUB
    fi
    exec dbus-run-session -- bash "$0" --in-session
fi

fails=0
check() { # name, condition result
    if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
png_size() { python3 -c 'import struct,sys; d=open(sys.argv[1],"rb").read(24); print("%dx%d" % struct.unpack(">II", d[16:24]))' "$1"; }
ms() { echo $(($(date +%s%N) / 1000000)); }
servers() { pgrep -f '^/usr/bin/telamon-screenshot|^telamon-screenshot' | tr '\n' ' '; }

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

S=telamon-screenshot
# These parts count the processes of the tool: no notification child.
[ "$E2E_PART" = modes ] || S="telamon-screenshot --no-notify"
if [ "$E2E_PART" = online ]; then
    # The old name (a link) is granted ScreenShot2 too: KWin goes by the real path.
    atlasos-screenshot --region 0,0,640x400; rc=$?
    check "old name: atlasos-screenshot (link) still copies through ScreenShot2" $rc
    wl-paste -t image/png >/out/oldname.png
    check "old name: clipboard has a 960x600 PNG" $([ "$(png_size /out/oldname.png)" = 960x600 ]; echo $?)
    wl-copy other
    sleep 1

    # Files of atlasos-screenshot (before 0.2.0) move on the first run.
    mkdir -p "$XDG_CONFIG_HOME/atlasos-screenshot"
    printf '[overlay]\ndim = 0.2\n' >"$XDG_CONFIG_HOME/atlasos-screenshot/config.toml"
    $S --region 0,0,100x100 2>/tmp/mig.txt; rc=$?
    check "migration: runs with the old config folder" $rc
    check "migration: config moved to telamon-screenshot" \
        $([ -f "$XDG_CONFIG_HOME/telamon-screenshot/config.toml" ] && [ ! -e "$XDG_CONFIG_HOME/atlasos-screenshot" ]; echo $?)
    check "migration: the moved config is valid (no warning)" $([ ! -s /tmp/mig.txt ]; echo $?)
    rm -r "$XDG_CONFIG_HOME/telamon-screenshot"
    wl-copy other
    sleep 1

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
    ls -la "$XDG_DATA_HOME/telamon-screenshot" "$XDG_DATA_HOME/telamon-screenshot/models"
    # Models of the old folder name move, and are used without a download.
    mv "$XDG_DATA_HOME/telamon-screenshot" "$XDG_DATA_HOME/atlasos-screenshot"
    $S --mode text --region 0,0,1280x800 2>/tmp/mig2.txt; rc=$?
    check "migration: text works from the old models folder" $rc
    check "migration: models moved, not downloaded again" \
        $([ -f "$XDG_DATA_HOME/telamon-screenshot/models/text-detection.rten" ] && [ ! -e "$XDG_DATA_HOME/atlasos-screenshot" ] && ! grep -qi download /tmp/mig2.txt; echo $?)
    t0=$(ms)
    $S --mode text --region 0,0,1280x800 && t1=$(ms) && echo "text: warm $((t1 - t0)) ms"

    $S --mode redact --region 0,0,1280x800; rc=$?
    check "redact: exit 0" $rc
    wl-paste -t image/png >/out/redact.png

    # Damage a model: it must be re-downloaded, not used.
    printf 'x' | dd of="$XDG_DATA_HOME/telamon-screenshot/models/text-detection.rten" bs=1 seek=100 conv=notrunc 2>/dev/null
    $S --mode text --region 0,0,1280x800; rc=$?
    check "text: damaged model is replaced" $rc

    # Bad config: defaults plus one warning, still works.
    mkdir -p "$XDG_CONFIG_HOME/telamon-screenshot"
    printf '[overlay]\ndim = 7\n' >"$XDG_CONFIG_HOME/telamon-screenshot/config.toml"
    $S --region 0,0,100x100 2>/tmp/warn.txt; rc=$?
    cat /tmp/warn.txt
    check "bad config: still copies" $rc
    check "bad config: one warning" $([ "$(grep -c overlay.dim /tmp/warn.txt)" = 1 ]; echo $?)
    rm "$XDG_CONFIG_HOME/telamon-screenshot/config.toml"

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
elif [ "$E2E_PART" = modes ]; then
    pics=$HOME/Pictures/Screenshots
    export MOCK_LOG=/out/notify.log MOCK_ACTION_FILE=/tmp/mock-action
    : >"$MOCK_ACTION_FILE"
    python3 /in/notify-mock.py &
    mock=$!
    waitfor() { # seconds, command...
        local n=$(($1 * 10)); shift
        while [ $n -gt 0 ]; do "$@" 2>/dev/null && return 0; sleep 0.1; n=$((n - 1)); done
        return 1
    }
    owned() { gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner "$1" | grep -q true; }
    waitfor 10 owned org.freedesktop.Notifications
    check "mock: the notification server is up" $?
    nn() { grep -c '"kind": "notify"' /out/notify.log; }
    notif() { # field of the last notification: summary, body, actions, hint:<name>
        python3 - "$1" <<'PY'
import json, sys
rows = [json.loads(l) for l in open("/out/notify.log") if '"kind": "notify"' in l]
r = rows[-1]
k = sys.argv[1]
v = r["hints"].get(k[5:]) if k.startswith("hint:") else r[k]
print(v if not isinstance(v, (list, dict)) else json.dumps(v))
PY
    }
    shot() { # args...: runs a capture, path (if saved) in $f, waits for its notification
        n0=$(nn)
        f=$($S "$@" 2>/tmp/shot.err); rc=$?
        waitfor 8 notified
    }
    notified() { [ "$(nn)" -gt "$n0" ]; }
    click() { echo "$1" >"$MOCK_ACTION_FILE"; waitfor 5 test ! -s "$MOCK_ACTION_FILE"; }
    # Same pixels? (the clipboard against a file)
    clip_is() { wl-paste -t image/png >/tmp/clip.png && cmp -s /tmp/clip.png "$1"; }
    named() { [[ $1 =~ ^$pics/Screenshot_[0-9]{8}_[0-9]{6}(-[0-9]+)?\.png$ ]]; }
    pngs() { ls "$pics" 2>/dev/null | wc -l; }

    # --- whole desktop, one screen
    shot --full
    check "full: exit 0" $rc
    check "full: prints a Screenshot_YYYYMMDD_HHMMSS.png in Pictures/Screenshots" $(named "$f"; echo $?)
    check "full: 1920x1200 (every screen)" $([ "$(png_size "$f")" = 1920x1200 ]; echo $?)
    check "full: the clipboard has the saved picture" $(clip_is "$f"; echo $?)
    check "full: saved private (0600 or 0640 at most)" $([ "$(( $(stat -c %a "$f") & 07 ))" = 0 ]; echo $?)
    check "full: notification Screenshot Saved" $([ "$(notif summary)" = "Screenshot Saved" ]; echo $?)
    check "full: notification shows the file" $([ "$(notif hint:image-path)" = "file://$f" ]; echo $?)
    check "full: notification buttons" $([ "$(notif actions)" = '["open", "Open", "folder", "Show in Folder", "edit", "Edit", "copy", "Copy"]' ]; echo $?)
    check "full: notification is Telamon Screenshot's" $([ "$(notif hint:desktop-entry)" = net.eterneon.telamon.screenshot ]; echo $?)
    cp "$f" /out/full.png
    first=$f
    shot --screen
    check "screen: exit 0, 1920x1200" $([ $rc = 0 ] && [ "$(png_size "$f")" = 1920x1200 ]; echo $?)
    check "screen: a new file, the first one untouched" $([ "$f" != "$first" ] && cmp -s "$first" /out/full.png; echo $?)

    # --- windows
    shot --active-window
    check "active window: exit 0 ($(head -c 200 /tmp/shot.err))" $rc
    w_default=$(png_size "$f")
    cp "$f" /out/window.png
    shot --active-window --no-frame --no-shadow
    w_bare=$(png_size "$f")
    cp "$f" /out/window-bare.png
    echo "active window: default $w_default, no frame and no shadow $w_bare"
    check "active window: smaller than the screen" $([ "$w_default" != 1920x1200 ]; echo $?)
    check "active window: frame and shadow make it bigger" $(python3 -c '
import sys
a=[int(x) for x in sys.argv[1].split("x")]; b=[int(x) for x in sys.argv[2].split("x")]
sys.exit(0 if a[0]>b[0] and a[1]>b[1] else 1)' "$w_default" "$w_bare"; echo $?)
    check "active window: has transparency (shadow)" $(python3 -c '
import sys
from PIL import Image
im = Image.open("/out/window.png")
sys.exit(0 if im.mode == "RGBA" and im.getchannel("A").getextrema()[0] < 255 else 1)'; echo $?)
    n0=$(pngs)
    timeout 4 $S --window; rc=$?
    check "window: waits for a click in KWin's picker" $([ $rc = 124 ]; echo $?)
    check "window: nothing saved when not picked" $([ "$(pngs)" = "$n0" ]; echo $?)
    shot --full
    check "after the picker: capturing still works" $rc

    # --- region without the overlay, and clicking the buttons
    shot --region 0,0,640x400
    check "region: exit 0, 960x600" $([ $rc = 0 ] && [ "$(png_size "$f")" = 960x600 ]; echo $?)
    rf=$f
    wl-copy other; sleep 0.5
    click copy
    waitfor 5 clip_is "$rf"
    check "button Copy: the picture is on the clipboard again" $?
    click open
    waitfor 5 grep -qxF "$rf" /out/xdg-open.log
    check "button Open: xdg-open gets the file as one argument" $?
    click folder
    waitfor 5 grep -qF "\"file://$rf\"" /out/notify.log
    check "button Show in Folder: FileManager1.ShowItems gets the file's URI" $?
    click edit
    waitfor 5 grep -qxF "$rf" /out/editor.log
    check "button Edit: the editor gets the file" $?

    # --- nothing kept
    n0=$(pngs)
    shot --full --no-save
    check "no-save: exit 0, no path printed, no file" $([ $rc = 0 ] && [ -z "$f" ] && [ "$(pngs)" = "$n0" ]; echo $?)
    check "no-save: the clipboard has a 1920x1200 picture" $(wl-paste -t image/png >/tmp/clip.png; [ "$(png_size /tmp/clip.png)" = 1920x1200 ]; echo $?)
    thumb_ok() { notif hint:image-data | python3 -c '
import json, sys
d = json.load(sys.stdin)
sys.exit(0 if 0 < d["w"] <= 320 and 0 < d["h"] <= 320 and d["bytes"] == d["w"] * d["h"] * 4 and d["alpha"] else 1)'; }
    check "no-save: notification Screenshot Copied, a thumbnail, only Edit and Copy" \
        $([ "$(notif summary)" = "Screenshot Copied" ] && [ "$(notif actions)" = '["edit", "Edit", "copy", "Copy"]' ] && thumb_ok; echo $?)
    wl-copy other; sleep 0.5
    click edit
    waitfor 5 test -s /out/editor-stdin.png
    check "no-save, button Edit: the editor gets the picture on stdin" $(cmp -s /out/editor-stdin.png /tmp/clip.png; echo $?)
    wl-copy other; sleep 0.5
    click copy
    waitfor 5 clip_is /tmp/clip.png
    check "no-save, button Copy: copied again from memory" $?

    # --- the editor flag opens the editor instead of notifying
    n0=$(nn); : >/out/editor.log
    f=$($S --full --edit); rc=$?
    waitfor 5 grep -qxF "$f" /out/editor.log
    check "--edit: the editor gets the saved file" $?
    check "--edit: no notification" $([ "$(nn)" = "$n0" ]; echo $?)
    rm -f /out/editor-stdin.png
    $S --full --edit --no-save; waitfor 5 test -s /out/editor-stdin.png
    check "--edit --no-save: the editor gets the picture on stdin" $?

    # --- config: do not save / do not notify / default mode / folder
    mkdir -p "$XDG_CONFIG_HOME/telamon-screenshot"
    printf '[output]\nsave = false\nnotify = false\n' >"$XDG_CONFIG_HOME/telamon-screenshot/config.toml"
    n0=$(pngs); m0=$(nn)
    f=$($S --full); rc=$?; sleep 1.5
    check "output.save = false: copies, saves nothing, says nothing" $([ $rc = 0 ] && [ "$(pngs)" = "$n0" ] && [ "$(nn)" = "$m0" ]; echo $?)
    printf '[capture]\ndefault_mode = "full"\n[output]\nsave_dir = "/tmp/elsewhere"\n' >"$XDG_CONFIG_HOME/telamon-screenshot/config.toml"
    mkdir -p -m755 /tmp/elsewhere
    f=$($S); rc=$?
    check "default_mode = full: a bare run needs no overlay, and output.save_dir is used" $([ $rc = 0 ] && [ "$(dirname "$f")" = /tmp/elsewhere ] && [ "$(png_size "$f")" = 1920x1200 ]; echo $?)
    mkdir -p -m777 /tmp/open; chmod 777 /tmp/open
    printf '[output]\nsave_dir = "/tmp/open"\n' >"$XDG_CONFIG_HOME/telamon-screenshot/config.toml"
    shot --full
    check "world-writable folder: refused, still copied, the notification says so" \
        $([ $rc = 0 ] && [ -z "$f" ] && [ -z "$(ls /tmp/open)" ] && [[ "$(notif body)" == *"other users can write"* ]]; echo $?)
    check "world-writable folder: the clipboard has the picture" $(wl-paste -t image/png >/tmp/clip.png; [ "$(png_size /tmp/clip.png)" = 1920x1200 ]; echo $?)
    rm "$XDG_CONFIG_HOME/telamon-screenshot/config.toml"

    # --- text is copied, never saved
    n0=$(pngs)
    shot --mode text --region 0,0,1280x800
    check "text: exit 0, nothing saved, notification Text Copied" $([ $rc = 0 ] && [ "$(pngs)" = "$n0" ] && [ "$(notif summary)" = "Text Copied" ]; echo $?)
    check "text: the notification doesn't show the text" $([[ "$(notif body)" != *"192.168"* ]]; echo $?)
    check "text: OCR found the address" $(wl-paste -t text/plain | grep -qE '192\.168\.1[. ]20'; echo $?)
    shot --mode redact --region 0,0,1280x800
    check "redact: saved and notified like a picture" $([ $rc = 0 ] && named "$f" && [ "$(notif summary)" = "Screenshot Saved" ]; echo $?)

    # --- names never collide
    a=$($S --region 0,0,64x64); b=$($S --region 0,0,64x64)
    check "two shots in the same second get two files" $([ -n "$a" ] && [ -n "$b" ] && [ "$a" != "$b" ] && [ -f "$a" ] && [ -f "$b" ]; echo $?)

    # --- delay, and the countdown is not in the picture
    $S --region 0,0,1280x800 >/dev/null; wl-paste -t image/png >/out/cd-before.png
    t0=$(ms)
    ($S --full --delay 4 >/out/cd-final.txt 2>/out/cd-final.err; echo $? >/out/cd-final.rc) &
    dpid=$!
    sleep 2
    $S --region 0,0,1280x800 >/dev/null; wl-paste -t image/png >/out/cd-during.png
    $S --full --delay 1 2>/tmp/second.txt; second=$?
    wait $dpid
    t1=$(ms)
    echo "delay: $((t1 - t0)) ms for --delay 4"
    check "delay: --delay 4 took at least 4 s" $([ $((t1 - t0)) -ge 4000 ]; echo $?)
    check "delay: it exited 0 and saved" $([ "$(cat /out/cd-final.rc)" = 0 ] && named "$(cat /out/cd-final.txt)"; echo $?)
    check "delay: a second capture during the countdown is refused (one at a time)" $([ $second = 1 ] && grep -q 'already in progress' /tmp/second.txt; echo $?)
    python3 - "$(cat /out/cd-final.txt)" >/out/cd-compare.txt <<'PY'
import sys
from PIL import Image, ImageChops
box = (888, 60, 1032, 204)  # the badge: 96 px wide at the top centre, 40 px down, at 1.5x
before = Image.open("/out/cd-before.png").convert("RGB").crop(box)
during = Image.open("/out/cd-during.png").convert("RGB").crop(box)
final = Image.open(sys.argv[1]).convert("RGB").crop(box)
def diff(a, b):
    return sum(sum(px) for px in ImageChops.difference(a, b).getdata())
print("badge-before-during", diff(before, during))
print("badge-before-final", diff(before, final))
PY
    cat /out/cd-compare.txt
    bd=$(awk '/before-during/ {print $2}' /out/cd-compare.txt); bf=$(awk '/before-final/ {print $2}' /out/cd-compare.txt)
    check "countdown: the badge is on screen during the wait" $([ "${bd:-0}" -gt 20000 ]; echo $?)
    check "countdown: and not in the picture taken after it" $([ "${bf:-1}" -lt 1000 ]; echo $?)

    # --- the helpers of the editor
    p=$(printf '' | $S --save-png 2>/tmp/sp.err); rc=$?
    check "--save-png: empty input is refused" $([ $rc = 1 ] && grep -q 'not a PNG' /tmp/sp.err; echo $?)
    p=$($S --save-png <"$first"); rc=$?
    check "--save-png: saves the PNG on stdin and prints its path" $([ $rc = 0 ] && named "$p" && cmp -s "$p" "$first"; echo $?)
    wl-copy other; sleep 0.5
    $S --copy-png <"$first"; rc=$?
    check "--copy-png: puts it on the clipboard" $([ $rc = 0 ] && clip_is "$first"; echo $?)
    echo 'not a png' | $S --copy-png 2>/dev/null; check "--copy-png: refuses what is not a PNG" $([ $? = 1 ]; echo $?)

    # --- org.kde.Spectacle
    dbus-monitor --session "type='signal',interface='org.kde.Spectacle'" >/out/spectacle-signals.log 2>&1 &
    mon=$!
    sleep 1
    gdbus call --session --dest org.kde.Spectacle --object-path / --method org.kde.Spectacle.FullScreen -- -1 >/dev/null 2>&1
    check "Spectacle D-Bus: FullScreen activates our service and returns" $?
    waitfor 15 grep -q ScreenshotTaken /out/spectacle-signals.log
    check "Spectacle D-Bus: ScreenshotTaken arrives" $?
    sp=$(grep -A1 ScreenshotTaken /out/spectacle-signals.log | grep -o 'string "[^"]*"' | head -1 | sed 's/string "//; s/"$//')
    check "Spectacle D-Bus: the signal names the saved 1920x1200 file" $([ -f "$sp" ] && named "$sp" && [ "$(png_size "$sp")" = 1920x1200 ]; echo $?)
    gdbus call --session --dest org.kde.Spectacle --object-path / --method org.kde.Spectacle.RecordScreen -- -1 >/dev/null 2>&1
    waitfor 5 grep -q RecordingFailed /out/spectacle-signals.log
    check "Spectacle D-Bus: recording answers RecordingFailed" $?
    gdbus introspect --session --dest org.kde.Spectacle --object-path / >/out/spectacle-introspect.txt 2>&1
    for m in StartAgent FullScreen CurrentScreen ActiveWindow WindowUnderCursor RectangularRegion RecordRegion RecordScreen RecordWindow OpenWithoutScreenshot; do
        grep -q "$m" /out/spectacle-introspect.txt || { echo "missing $m"; fails=$((fails + 1)); }
    done
    gdbus call --session --dest org.kde.Spectacle --object-path / --method org.kde.Spectacle.ActiveWindow -- 0 0 0 >/dev/null 2>&1
    two_taken() { [ "$(grep -c ScreenshotTaken /out/spectacle-signals.log)" -ge 2 ]; }
    waitfor 15 two_taken
    check "Spectacle D-Bus: ActiveWindow(0,0,0) answers too" $?
    kill $mon 2>/dev/null
    kill $mock 2>/dev/null
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

pkill -f telamon-screenshot
kill $kwin
echo "$E2E_PART: $fails failure(s)"
exit $fails
