#!/bin/bash
# Run a command in the Qt build container of the annotation editor, with the
# repo at /src and the build output at /work.
#   scripts/dev-editor.sh <command...>   e.g. scripts/dev-editor.sh editor/build.sh
#   scripts/dev-editor.sh                an interactive shell
# /work is $TELAMON_EDITOR_WORK, by default
# ~/.cache/claude-builds/telamon-screenshot-editor.
# The image is localhost/telamon-screenshot-editor-dev:44, made once (and
# cached) from $TELAMON_EDITOR_BASE (default localhost/telamon-notepad-dev:44):
# a Fedora 44 image with telamon-ui, Qt 6.11 development files, cmake and
# ninja, which no repository ships (telamon-ui comes from the framework's RPMs).
# Anything the editor needs on top (Qt Quick Test, ImageMagick, Xvfb) is
# installed into the derived image when the base lacks it.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
image=localhost/telamon-screenshot-editor-dev:44
base=${TELAMON_EDITOR_BASE:-localhost/telamon-notepad-dev:44}
work=${TELAMON_EDITOR_WORK:-$HOME/.cache/claude-builds/telamon-screenshot-editor}
mkdir -p "$work"

if ! podman image exists "$image"; then
    podman image exists "$base" || {
        echo "dev-editor.sh: base image $base is missing (set TELAMON_EDITOR_BASE)" >&2
        exit 1
    }
    # No SELinux relabelling (:z/:Z) of host folders: labels are off instead.
    ctr=$(podman run -d --init --security-opt label=disable \
        -v telamon-dnf:/var/cache/libdnf5 "$base" sleep infinity)
    trap 'podman rm -f -t 0 "$ctr" >/dev/null' EXIT
    podman exec "$ctr" bash -c '
        set -e
        need=()
        for p in qt6-qtbase-devel qt6-qtdeclarative-devel cmake ninja-build \
                 telamon-ui xorg-x11-server-Xvfb ImageMagick; do
            rpm -q "$p" >/dev/null 2>&1 || need+=("$p")
        done
        [ "${#need[@]}" = 0 ] || { echo keepcache=True >>/etc/dnf/dnf.conf; dnf -y install "${need[@]}"; }' >&2
    podman commit "$ctr" "$image" >/dev/null
    podman rm -f -t 0 "$ctr" >/dev/null
    trap - EXIT
fi

# TELAMON_EDITOR_SCHEMES: a folder with BreezeLight.colors and BreezeDark.colors
# (the framework's tests/visual/schemes), for scripts/editor-shots.sh.
schemes=()
[ -n "${TELAMON_EDITOR_SCHEMES:-}" ] && schemes=(-v "$TELAMON_EDITOR_SCHEMES":/schemes:ro)

tty=()
[ -t 0 ] && tty=(-it)
exec podman run --rm --init "${tty[@]}" --security-opt label=disable --ulimit core=0 \
    -v "$repo":/src -w /src \
    -v "$work":/work "${schemes[@]}" \
    -e QT_QPA_PLATFORM="${QT_QPA_PLATFORM:-offscreen}" \
    -e QT_FORCE_STDERR_LOGGING=1 \
    "$image" "${@:-bash}"
