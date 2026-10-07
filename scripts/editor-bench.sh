#!/bin/bash
# Cold start (process start to the first frame, ms) and memory (RSS idle and
# peak, MB) of the editor, median of N runs, on the offscreen platform and the
# software renderer. Run in scripts/dev-editor.sh after editor/build.sh:
#   scripts/dev-editor.sh scripts/editor-bench.sh [runs]
# Prints a table; add it to editor/benchmarks.md (gitignored).
set -euo pipefail
n=${1:-7}
b=/work/build/telamon-screenshot-editor-test
t=/src/editor/tests
out=/work/shots
mkdir -p "$out"
[ -f "$out/sample.png" ] || "$t/make-sample.sh" "$out/sample.png" 2>/dev/null
[ -f "$out/sample4k.png" ] || "$t/make-sample.sh" "$out/sample4k.png" 3840x2160 2>/dev/null
export QT_QUICK_BACKEND=software
median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }
run() { # label, args...
    local label=$1; shift
    local ms=() rss=() hwm=()
    for ((i = 0; i < n; i++)); do
        local start=$(date +%s%3N)
        read -r t0 r h < <("$b" --first-frame "$@" 2>/dev/null | tail -1)
        ms+=($((t0 - start))); rss+=($r); hwm+=($h)
    done
    printf '%-26s start %5s ms   RSS %6.1f MB   peak %6.1f MB\n' "$label" \
        "$(printf '%s\n' "${ms[@]}" | median)" \
        "$(printf '%s\n' "${rss[@]}" | median | awk '{print $1/1024}')" \
        "$(printf '%s\n' "${hwm[@]}" | median | awk '{print $1/1024}')"
}
run "empty window"
run "1600x900 image" "$out/sample.png"
run "3840x2160 image" "$out/sample4k.png"
ls -l /work/build/telamon-screenshot-editor | awk '{printf "binary (unstripped): %.2f MB\n", $5/1048576}'
strip -o /tmp/editor.stripped /work/build/telamon-screenshot-editor
ls -l /tmp/editor.stripped | awk '{printf "binary (stripped):   %.2f MB\n", $5/1048576}'
