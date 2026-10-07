#!/bin/bash
# A stand-in for telamon-screenshot in the tests (TELAMON_SCREENSHOT_BIN).
# It writes into $FAKE_CLI_DIR, and logs its arguments to $FAKE_CLI_DIR/args.log.
#   --save-png   PNG on stdin -> $FAKE_CLI_DIR/Screenshot_N.png, prints the path
#   --copy-png   PNG on stdin -> $FAKE_CLI_DIR/clipboard.png
#   FAKE_CLI_FAIL=text  makes both fail with that text on stderr, exit 1
dir=${FAKE_CLI_DIR:-${TMPDIR:-/tmp}}
echo "$*" >>"$dir/args.log"
if [ -n "${FAKE_CLI_FAIL:-}" ]; then
    echo "$FAKE_CLI_FAIL" >&2
    exit 1
fi
case "$1" in
--save-png)
    n=$(ls "$dir"/Screenshot_*.png 2>/dev/null | wc -l)
    out="$dir/Screenshot_$((n + 1)).png"
    cat >"$out"
    echo "$out"
    ;;
--copy-png)
    cat >"$dir/clipboard.png"
    ;;
*)
    # --region, --full, ... --edit: a capture; nothing to do here.
    ;;
esac
