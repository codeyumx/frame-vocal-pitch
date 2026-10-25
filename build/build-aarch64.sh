#!/usr/bin/env bash
# Build the project for the Steam Frame (aarch64-unknown-linux-gnu) inside the
# cross-compile container. Extra arguments go to `cargo build`.
#   build/build-aarch64.sh --release
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
image=frame-pitch-build:noble

docker build -q -t "$image" "$here" >/dev/null

mkdir -p "$root/target/.cargo-registry"
docker run --rm \
  -u "$(id -u):$(id -g)" \
  -v "$root:/src" \
  -v "$root/target/.cargo-registry:/opt/cargo/registry" \
  "$image" \
  cargo build --target aarch64-unknown-linux-gnu "$@"
