#!/bin/bash
# Build the Telamon Screenshot RPM inside a fedora:44 container, as root.
#   packaging/build-rpm.sh <out dir> [rpmbuild options]
# Works from a plain copy of the tree (no .git needed). The binary RPM is
# copied to <out dir> (telamon-screenshot and telamon-screenshot-spectacle-compat).
# Cargo needs network access.
# TELAMON_LOCAL_RPMS=<dir> installs the RPMs in <dir> first: telamon-framework's
# (telamon-ui and what it needs), which the editor builds against and no
# repository has. ATLAS_LOCAL_RPMS is the same, by its old name.
# TELAMON_SKIP_DEPS=1 skips every dnf and rpm install: the machine must have
# them already, as CI's build image does.
set -euo pipefail

main() {
    out=${1:?usage: build-rpm.sh <out dir> [rpmbuild options]}
    shift

    here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
    src=$(dirname "$here")
    spec=$here/telamon-screenshot.spec
    version=$(awk '/^Version:/ {print $2; exit}' "$spec")

    local_rpms_dir=${TELAMON_LOCAL_RPMS:-${ATLAS_LOCAL_RPMS:-}}
    if [ "${TELAMON_SKIP_DEPS:-}" != 1 ]; then
        dnf -y install rpm-build dnf5-plugins tar gzip >&2
        if [ -n "$local_rpms_dir" ]; then
            local_rpms=()
            for f in "$local_rpms_dir"/*.rpm; do
                [ -e "$f" ] && [[ $f != *.src.rpm ]] && local_rpms+=("$f")
            done
            if ! printf '%s\n' "${local_rpms[@]:-}" | grep -q '/telamon-ui-[0-9]'; then
                echo "$local_rpms_dir has no telamon-ui RPM" >&2
                exit 1
            fi
            # The files of exactly these RPMs, even when that version is installed.
            dnf -y install "${local_rpms[@]}" >&2
            rpm -U --replacepkgs --oldpackage "${local_rpms[@]}" >&2
        fi
        dnf -y builddep "$spec" >&2
    fi

    top=$(mktemp -d)
    trap 'rm -rf "$top"' EXIT
    mkdir -p "$top"/{SOURCES,BUILD,RPMS,SRPMS,SPECS}
    tar -C "$src" \
        --exclude=./.git --exclude=./target --exclude=./out --exclude=./build \
        --transform "s,^\./,telamon-screenshot-$version/," \
        -czf "$top/SOURCES/telamon-screenshot-$version.tar.gz" .

    rpmbuild -bb "$@" --define "_topdir $top" "$spec"

    mkdir -p "$out"
    find "$top/RPMS" -name '*.rpm' ! -name '*.src.rpm' ! -name '*debuginfo*' ! -name '*debugsource*' \
        -exec cp -v {} "$out"/ \;
}

main "$@"
exit $?
