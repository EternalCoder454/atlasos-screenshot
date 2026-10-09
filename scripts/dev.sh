#!/bin/bash
# Run a command in the fedora:44 build container, with the repo at /src, the
# build output at /work and the cargo cache in the podman volumes shared with
# the other Telamon apps.
#   scripts/dev.sh <command...>     e.g. scripts/dev.sh cargo test --locked
#   scripts/dev.sh                  an interactive shell
# /work is $TELAMON_SCREENSHOT_WORK, by default
# ~/.cache/claude-builds/telamon-screenshot. Set CARGO_TARGET_DIR to
# /work/target/<name> to keep one target dir per task.
# The first run builds localhost/telamon-screenshot-dev:44 (cached after).
# Delete it after changing the spec's BuildRequires.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
image=localhost/telamon-screenshot-dev:44
work=${TELAMON_SCREENSHOT_WORK:-$HOME/.cache/claude-builds/telamon-screenshot}
mkdir -p "$work"

if ! podman image exists "$image"; then
    # No SELinux relabelling (:z/:Z) of host folders: labels are off instead.
    ctr=$(podman run -d --init --security-opt label=disable \
        -v "$repo/packaging":/packaging:ro \
        -v telamon-dnf:/var/cache/libdnf5 \
        registry.fedoraproject.org/fedora:44 sleep infinity)
    trap 'podman rm -f "$ctr" >/dev/null' EXIT
    podman exec "$ctr" bash -c '
        echo keepcache=True >>/etc/dnf/dnf.conf
        dnf -y install dnf5-plugins rpm-build clippy rustfmt &&
        dnf -y builddep /packaging/telamon-screenshot.spec' >&2
    podman commit "$ctr" "$image" >/dev/null
    podman rm -f "$ctr" >/dev/null
    trap - EXIT
fi

tty=()
[ -t 0 ] && tty=(-it)
# core=0: a crash must not leave a core dump (screen contents) in the system store.
exec podman run --rm --init "${tty[@]}" --security-opt label=disable --ulimit core=0 \
    -v "$repo":/src -w /src \
    -v "$work":/work \
    -v telamon-cargo:/root/.cargo/registry \
    -v telamon-cargo-git:/root/.cargo/git \
    -e CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/work/target/dev}" \
    "$image" "${@:-bash}"
