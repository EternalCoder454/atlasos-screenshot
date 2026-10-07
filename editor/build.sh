#!/bin/bash
# Configure and build the editor and its tests, then run them. Meant to run in
# the container of scripts/dev-editor.sh (repo at /src, output at /work):
#   scripts/dev-editor.sh editor/build.sh [ctest args]
set -euo pipefail
src=${EDITOR_SRC:-/src/editor}
build=${EDITOR_BUILD:-/work/build}
cmake -S "$src" -B "$build" -G Ninja -DTELAMON_EDITOR_TESTS=ON -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "$build" -j"${JOBS:-8}"
cd "$build" && ctest --output-on-failure "$@"
