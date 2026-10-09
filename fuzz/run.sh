#!/bin/bash
# Runs the fuzz targets for a while each, on nightly with cargo-fuzz.
#   fuzz/run.sh [seconds per target, default 30] [target...]
# The seeds in fuzz/corpus/<target> and the inputs that crashed a target once
# (fuzz/regressions/<target>, each a fixed bug or a finding waiting for its fix)
# are only read: what the fuzzer finds goes to $FUZZ_WORK/corpus/<target>
# (default fuzz/work, not checked in) and a crash to
# $FUZZ_WORK/artifacts/<target>/ (CI uploads that folder). Exits non-zero when
# any target crashed, and after trying all of them.
#   FUZZ_SANITIZER=none   skips AddressSanitizer (faster, no memory errors in unsafe code)
set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
secs=${1:-30}
shift || true
work=${FUZZ_WORK:-$here/work}
sanitizer=${FUZZ_SANITIZER:-address}

# target:dictionary:max_len
table=(
    post_decode:job:65536
    cli_args:cli:4096
    region:cli:256
    user_dirs:dirs:8192
    config_parse:config:16384
    file_uri:uri:1024
    stamp_names:none:9
    png_header:png:4096
    redact_text:redact:2048
    comm_label:cli:64
)

want=("$@")
failed=()
cd "$here"
for row in "${table[@]}"; do
    IFS=: read -r target dict max_len <<<"$row"
    if [ "${#want[@]}" -gt 0 ] && [[ ! " ${want[*]} " == *" $target "* ]]; then
        continue
    fi
    mkdir -p "$work/corpus/$target" "$work/artifacts/$target"
    extra=()
    [ -d "$here/regressions/$target" ] && extra=("$here/regressions/$target")
    echo "=== $target (${secs}s)"
    if ! cargo fuzz run --sanitizer "$sanitizer" "$target" "$work/corpus/$target" "$here/corpus/$target" "${extra[@]}" -- \
        -max_total_time="$secs" -dict="$here/dictionaries/$dict.dict" -max_len="$max_len" \
        -timeout=10 -rss_limit_mb=2048 -artifact_prefix="$work/artifacts/$target/" -print_final_stats=1; then
        failed+=("$target")
    fi
done
if [ "${#failed[@]}" -gt 0 ]; then
    echo "FAILED: ${failed[*]}" >&2
    exit 1
fi
