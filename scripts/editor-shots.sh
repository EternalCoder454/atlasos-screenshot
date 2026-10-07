#!/bin/bash
# Screenshots of the editor, headless (offscreen platform, the real Telamon.Ui),
# in light and dark: the empty state, every tool used once, and the crop
# handles. Run inside scripts/dev-editor.sh after editor/build.sh:
#   scripts/dev-editor.sh scripts/editor-shots.sh [scale]
# The PNGs land in /work/shots; scale (default 1.5) is QT_SCALE_FACTOR.
set -euo pipefail
scale=${1:-1.5}
b=/work/build/telamon-screenshot-editor-test
t=/src/editor/tests
out=/work/shots
mkdir -p "$out"
[ -f "$out/sample.png" ] || "$t/make-sample.sh" "$out/sample.png" 2>/dev/null
# The colour scheme is the desktop's: a kdeglobals file with the Breeze Light or
# Dark colours (TELAMON_EDITOR_SCHEMES, mounted at /schemes), as the
# framework's own visual tests do.
export QT_SCALE_FACTOR=$scale QT_QUICK_BACKEND=software QT_QPA_PLATFORMTHEME=
for mode in light dark; do
    cfg=/tmp/cfg-$mode
    mkdir -p "$cfg"
    scheme=/schemes/Breeze${mode^}.colors
    if [ -f "$scheme" ]; then cp "$scheme" "$cfg/kdeglobals"; else echo "no $scheme: the default scheme is used" >&2; fi
    export XDG_CONFIG_HOME=$cfg
    "$b" --screenshot "$out/editor-$mode-empty.png"
    "$b" --screenshot "$out/editor-$mode-all-tools.png" --scenario "$t/scenario-all-tools.js" "$out/sample.png"
    "$b" --screenshot "$out/editor-$mode-crop.png" --scenario "$t/scenario-crop.js" "$out/sample.png"
    for s in popover saveas discard toast; do
        "$b" --screenshot "$out/editor-$mode-$s.png" --scenario "$t/scenario-$s.js" "$out/sample.png"
    done
done
