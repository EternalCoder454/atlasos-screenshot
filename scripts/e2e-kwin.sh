#!/bin/bash
# End-to-end run inside the Telamon OS image: the image's real KWin (virtual
# backend, 1.5x scale), ScreenShot2 with the installed .desktop file, the real
# clipboard, the real model download, and the overlay.
#   scripts/e2e-kwin.sh [binary]
# binary: default ~/.cache/claude-builds/telamon-screenshot/target/release/telamon-screenshot
# Results: ~/.cache/claude-builds/telamon-screenshot/e2e/ (e2e.log, PNGs).
# The OCR models are cached in e2e/data between runs. Selection by mouse is
# not driven here (KWin's virtual backend has no input injection for us); the
# overlay is checked by capturing the screen while it is up.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=${TELAMON_SCREENSHOT_WORK:-$HOME/.cache/claude-builds/telamon-screenshot}
bin=${1:-$work/target/release/telamon-screenshot}
image=${TELAMON_IMAGE:-${ATLAS_IMAGE:-localhost/atlasos:mon7}}
out=$work/e2e
mkdir -p "$out/data"
rm -f "$out"/*.png "$out"/*.txt "$out"/*.log

# KWin's screenshot effect needs OpenGL compositing; without a render node
# the virtual backend falls back to QPainter and every capture is cancelled.
gpu=()
[ -e /dev/dri/renderD128 ] && gpu=(--device /dev/dri/renderD128)

run() {
    podman run --rm --init --security-opt label=disable "${gpu[@]}" "$@" \
        -v "$bin":/in/telamon-screenshot:ro \
        -v "$repo/data":/in/data:ro \
        -v "$repo/scripts/e2e-inner.sh":/in/e2e-inner.sh:ro \
        -v "$out":/out \
        "$image" bash /in/e2e-inner.sh
}

rc=0
run -e E2E_PART=online || rc=1
# No network and no models: Ctrl/Alt must fail cleanly, plain must work.
run --network none -e E2E_PART=offline || rc=1
echo "results in $out"
exit $rc
