# Cross-compile environment for the Steam Frame (aarch64 SteamOS, glibc 2.39).
#
# Runs at native x86_64 speed and links against Ubuntu 24.04's arm64 libraries.
# Ubuntu 24.04 ships glibc 2.39, the same as the Frame, so the binary never asks
# for newer glibc symbols than the headset has.
FROM ubuntu:24.04

ARG RUST_VERSION=stable
ENV DEBIAN_FRONTEND=noninteractive

# amd64 packages come from the main archive and arm64 packages from ports.
RUN sed -i 's/^Types: deb$/Types: deb\nArchitectures: amd64/' /etc/apt/sources.list.d/ubuntu.sources \
 && printf '%s\n' \
      'Types: deb' \
      'URIs: http://ports.ubuntu.com/ubuntu-ports' \
      'Suites: noble noble-updates noble-security' \
      'Components: main universe' \
      'Architectures: arm64' \
      'Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg' \
      > /etc/apt/sources.list.d/ports-arm64.sources \
 && dpkg --add-architecture arm64 \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git pkg-config clang libclang-dev \
      gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
      libpipewire-0.3-dev:arm64 libspa-0.2-dev:arm64 libvulkan-dev:arm64 \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_VERSION} \
      --target aarch64-unknown-linux-gnu \
 && chmod -R a+rwX /opt/rustup /opt/cargo

# Cross linker, pkg-config pointed at the arm64 .pc files, and bindgen pointed
# at the arm64 sysroot so generated PipeWire bindings use aarch64 layouts.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    PKG_CONFIG_ALLOW_CROSS=1 \
    PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig \
    PKG_CONFIG_SYSROOT_DIR=/ \
    BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="--target=aarch64-linux-gnu -I/usr/include/aarch64-linux-gnu"

WORKDIR /src
