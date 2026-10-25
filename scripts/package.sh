#!/usr/bin/env bash
# Builds a release tarball for the Steam Frame (SPEC §7).
#
#   scripts/package.sh VERSION PATH_TO_BINARY
#   scripts/package.sh 1.2.3 target/aarch64-unknown-linux-gnu/release/frame-vocal-pitch
#
# Writes dist/frame-vocal-pitch-VERSION-aarch64.tar.gz containing one top-level directory
# frame-vocal-pitch-VERSION/ with the binary, install.sh, the systemd unit, the .desktop file,
# icons/{48,128,256}.png, LICENSE, THIRD_PARTY_LICENSES.md and README.md, plus dist/SHA256SUMS.
#
# The archive is reproducible: entries are sorted, owned by root, mode-normalised and stamped
# with SOURCE_DATE_EPOCH (default: the mtime of the newest input, which in CI is the checkout;
# set SOURCE_DATE_EPOCH explicitly to reproduce a build byte for byte).
#
# Testing only:
#   FVP_PACKAGE_ALLOW_ARCH=1   accept a binary that `file` does not recognise as aarch64
#   DIST_DIR=DIR               write the artefacts somewhere other than dist/
set -euo pipefail
umask 022

APP=frame-vocal-pitch
ICON_SIZES=(48 128 256)

say() { printf '%s\n' "$*"; }
die() {
    printf '%s package: error: %s\n' "$APP" "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: scripts/package.sh VERSION PATH_TO_BINARY

  VERSION          release version without the leading "v", e.g. 1.2.3
  PATH_TO_BINARY   aarch64 frame-vocal-pitch executable
EOF
}

[ $# -eq 2 ] || {
    usage >&2
    exit 2
}

version=${1#v}
binary=$2

[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.+][0-9A-Za-z.+-]+)?$ ]] ||
    die "version must look like 1.2.3 (got '$1')"
[ -f "$binary" ] || die "no such binary: $binary"
[ -x "$binary" ] || die "binary is not executable: $binary"
if command -v file >/dev/null; then
    desc=$(file -b "$binary")
    case $desc in
        *aarch64*) ;;
        *)
            if [ "${FVP_PACKAGE_ALLOW_ARCH:-}" = 1 ]; then
                say "note: $binary is not aarch64 ($desc); allowed by FVP_PACKAGE_ALLOW_ARCH=1"
            else
                die "$binary is not an aarch64 executable ($desc)"
            fi
            ;;
    esac
fi

root=$(cd "$(dirname "$0")/.." && pwd)
dist=${DIST_DIR:-$root/dist}
name=$APP-$version
folder=$name

required=(
    "$root/packaging/install.sh"
    "$root/packaging/$APP.service"
    "$root/packaging/$APP.desktop"
    "$root/LICENSE"
    "$root/THIRD_PARTY_LICENSES.md"
    "$root/README.md"
)
for n in "${ICON_SIZES[@]}"; do
    required+=("$root/assets/icons/$n.png")
done
for f in "${required[@]}"; do
    [ -f "$f" ] || die "missing release file: ${f#"$root/"}"
done

staging=$(mktemp -d "${TMPDIR:-/tmp}/$APP-package.XXXXXX")
trap 'rm -rf "$staging"' EXIT

payload=$staging/$folder
mkdir -p "$payload/icons" "$dist"

install -m 0755 "$binary" "$payload/$APP"
# install.sh carries the version it installs and lists from --version.
sed "s/@VERSION@/$version/g" "$root/packaging/install.sh" >"$payload/install.sh"
chmod 0755 "$payload/install.sh"
if grep -q '@VERSION@' "$payload/install.sh"; then
    die "install.sh still contains the @VERSION@ placeholder"
fi
install -m 0644 "$root/packaging/$APP.service" "$payload/$APP.service"
install -m 0644 "$root/packaging/$APP.desktop" "$payload/$APP.desktop"
for n in "${ICON_SIZES[@]}"; do
    install -m 0644 "$root/assets/icons/$n.png" "$payload/icons/$n.png"
done
install -m 0644 "$root/LICENSE" "$payload/LICENSE"
install -m 0644 "$root/THIRD_PARTY_LICENSES.md" "$payload/THIRD_PARTY_LICENSES.md"
install -m 0644 "$root/README.md" "$payload/README.md"

# Reproducible tar: sorted entries, fixed ownership, fixed mtime, no gzip timestamp.
epoch=${SOURCE_DATE_EPOCH:-}
if [ -z "$epoch" ]; then
    epoch=$(cd "$root" && git log -1 --format=%ct 2>/dev/null || true)
    epoch=${epoch:-0}
fi

tarball=$APP-$version-aarch64.tar.gz
tmp_tar=$staging/$tarball
tar --create --file - \
    --directory "$staging" \
    --sort=name \
    --mtime="@$epoch" \
    --owner=0 --group=0 --numeric-owner \
    --format=gnu \
    "$folder" | gzip -n -9 >"$tmp_tar"

mv -f "$tmp_tar" "$dist/$tarball"
(
    cd "$dist"
    sha256sum "$tarball" >SHA256SUMS
)

say "Wrote $dist/$tarball"
say "Wrote $dist/SHA256SUMS"
cat "$dist/SHA256SUMS"
