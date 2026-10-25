#!/usr/bin/env bash
# Installer for frame-vocal-pitch (vocal pitch overlay for SteamVR on the Steam Frame).
#
#   curl -fsSL https://github.com/codeyumx/frame-vocal-pitch/releases/latest/download/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- --autostart
#
# Options:
#   --autostart        start with SteamVR (enable the systemd user unit)
#   --no-autostart     do not start with SteamVR (disable the unit)
#   --uninstall        remove everything except the configuration
#   --uninstall --purge  also remove ~/.config/frame-vocal-pitch
#   --version          print the version of the payload and exit
#   --help             print this help
#
# Run from an extracted release folder it installs that folder's payload. Run any other way
# (piped, or as a lone file) it asks the GitHub API which release is latest, downloads that
# release's tarball and SHA256SUMS, verifies the checksum, extracts to a temporary directory and
# re-runs this installer from there. Re-running updates in place; the autostart choice is kept in
# ~/.config/frame-vocal-pitch/install-args.
#
# Nothing outside your home directory is touched. PipeWire, WirePlumber and the ALSA mixer are
# never restarted or written to.
#
# Testing only (never needed on the headset):
#   FVP_INSTALL_ALLOW_ARCH=1      skip the aarch64 check (dry runs on a PC with a fake payload)
#   FVP_INSTALL_DOWNLOAD_URL=URL  base URL holding SHA256SUMS and the tarball, bypassing the
#                                 GitHub API (a file:// directory works for dry runs)
#   FVP_INSTALL_REPO=owner/repo   GitHub repository to resolve the release from
set -euo pipefail

APP=frame-vocal-pitch
UNIT=$APP.service
PAYLOAD_VERSION='@VERSION@'
GITHUB_REPO=${FVP_INSTALL_REPO:-codeyumx/$APP}
RELEASES_URL=https://github.com/$GITHUB_REPO/releases

BIN_DIR=$HOME/.local/bin
UNIT_DIR=$HOME/.config/systemd/user
APPS_DIR=$HOME/.local/share/applications
ICON_ROOT=$HOME/.local/share/icons/hicolor
CONFIG_DIR=$HOME/.config/$APP
INSTALL_ARGS=$CONFIG_DIR/install-args
ICON_SIZES=(48 128 256)

# systemctl --user needs the real runtime dir; in Desktop Mode XDG_RUNTIME_DIR may point at a
# nested session where it fails.
RUNTIME_DIR=/run/user/$(id -u)

say() { printf '%s\n' "$*"; }
warn() { printf '%s: warning: %s\n' "$APP install" "$*" >&2; }
die() {
    printf '%s: error: %s\n' "$APP install" "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: install.sh [--autostart | --no-autostart]
       install.sh --uninstall [--purge]
       install.sh --version

  --autostart      start with SteamVR (enable the systemd user unit)
  --no-autostart   do not start with SteamVR
  --uninstall      remove the app (keeps ~/.config/frame-vocal-pitch)
  --purge          with --uninstall: also remove ~/.config/frame-vocal-pitch
  --version        print the version of the payload
  -h, --help       print this help
EOF
}

user_systemctl() {
    XDG_RUNTIME_DIR=$RUNTIME_DIR systemctl --user "$@"
}

check_environment() {
    if [ "$(id -u)" -eq 0 ]; then
        die "do not run as root or with sudo; it installs into your home directory"
    fi
    local arch
    arch=$(uname -m)
    if [ "$arch" != aarch64 ]; then
        if [ "${FVP_INSTALL_ALLOW_ARCH:-}" = 1 ]; then
            warn "architecture is $arch, not aarch64 (allowed by FVP_INSTALL_ALLOW_ARCH=1)"
        else
            die "this build is for the Steam Frame (aarch64); this machine is $arch"
        fi
    fi
    local os_id=""
    if [ -r /etc/os-release ]; then
        # shellcheck source=/dev/null
        os_id=$(. /etc/os-release && printf '%s' "${ID:-}")
    fi
    if [ "$os_id" != steamos ]; then
        warn "this does not look like SteamOS (ID=${os_id:-unknown}); continuing anyway"
    fi
}

# Directory of this script if it sits in an extracted release, else empty.
payload_dir() {
    local src=${BASH_SOURCE[0]:-}
    [ -n "$src" ] && [ -f "$src" ] || return 0
    local dir
    dir=$(cd "$(dirname "$src")" && pwd)
    if [ -f "$dir/$APP" ] && [ -f "$dir/$UNIT" ] && [ -f "$dir/$APP.desktop" ]; then
        printf '%s\n' "$dir"
    fi
}

payload_version() {
    # The placeholder is split so that packaging's substitution leaves this test intact.
    if [ "$PAYLOAD_VERSION" = "@""VERSION@" ]; then
        printf 'unknown (unpackaged copy)\n'
    else
        printf '%s\n' "$PAYLOAD_VERSION"
    fi
}

# Base URL holding SHA256SUMS and the tarball. Asks the GitHub API for the latest release tag;
# when the API is unreachable or rate limited, falls back to the "latest release" redirect, which
# needs no API call. The tag is used to pin both downloads to the same release.
resolve_download_url() {
    if [ -n "${FVP_INSTALL_DOWNLOAD_URL:-}" ]; then
        printf '%s\n' "${FVP_INSTALL_DOWNLOAD_URL%/}"
        return 0
    fi
    local json="" tag=""
    json=$(curl -fsSL --retry 2 --max-time 20 \
        -H 'Accept: application/vnd.github+json' \
        "https://api.github.com/repos/$GITHUB_REPO/releases/latest" 2>/dev/null) || json=""
    tag=$(printf '%s\n' "$json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    tag=${tag%%$'\n'*}
    if [[ $tag =~ ^[vV]?[0-9] ]]; then
        printf '%s/download/%s\n' "$RELEASES_URL" "$tag"
    else
        warn "cannot ask the GitHub API for the latest release; using the latest-release redirect"
        printf '%s/latest/download\n' "$RELEASES_URL"
    fi
}

# Downloads the latest release, verifies it and runs its installer with the same arguments.
download_and_run() {
    if [ -n "${FVP_INSTALL_REEXEC:-}" ]; then
        die "the downloaded release does not contain a complete payload"
    fi
    command -v curl >/dev/null || die "curl is required"
    command -v sha256sum >/dev/null || die "sha256sum is required"
    command -v tar >/dev/null || die "tar is required"

    local tmp
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/$APP-install.XXXXXX")
    # shellcheck disable=SC2064 # expand now: $tmp is local
    trap "rm -rf '$tmp'" EXIT

    local base
    base=$(resolve_download_url)
    say "Downloading $base/SHA256SUMS"
    curl -fsSL --retry 3 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" ||
        die "cannot download SHA256SUMS"

    local sum name tarball="" expected=""
    while read -r sum name; do
        name=${name#\*}
        if [[ $name =~ ^$APP-[0-9][0-9A-Za-z.+-]*-aarch64\.tar\.gz$ ]]; then
            tarball=$name
            expected=$sum
            break
        fi
    done <"$tmp/SHA256SUMS"
    [ -n "$tarball" ] || die "SHA256SUMS lists no $APP-*-aarch64.tar.gz"

    say "Downloading $base/$tarball"
    curl -fsSL --retry 3 -o "$tmp/$tarball" "$base/$tarball" ||
        die "cannot download $tarball"

    local actual
    actual=$(sha256sum "$tmp/$tarball")
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || die "checksum mismatch for $tarball (expected $expected, got $actual)"
    say "Checksum OK"

    tar -xzf "$tmp/$tarball" -C "$tmp" || die "cannot extract $tarball"
    local dir=$tmp/${tarball%-aarch64.tar.gz}
    [ -f "$dir/install.sh" ] || die "$tarball has no ${dir##*/}/install.sh"

    local status=0
    FVP_INSTALL_REEXEC=1 bash "$dir/install.sh" "$@" || status=$?
    exit "$status"
}

# PID of a running frame-vocal-pitch from a lock file, if that process really is ours.
instance_pid() {
    local lock=$1 pid="" cmd=""
    [ -r "$lock" ] || return 0
    read -r pid <"$lock" || true
    [[ $pid =~ ^[1-9][0-9]*$ ]] || return 0
    [ -r "/proc/$pid/cmdline" ] || return 0
    IFS= read -r -d '' cmd <"/proc/$pid/cmdline" || true
    if [ "${cmd##*/}" = "$APP" ]; then
        printf '%s\n' "$pid"
    fi
}

# Stops an instance started by hand (not by systemd), found through the PID in its lock file.
stop_manual_instance() {
    local locks=("$RUNTIME_DIR/$APP.lock")
    if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ "$XDG_RUNTIME_DIR" != "$RUNTIME_DIR" ]; then
        locks+=("$XDG_RUNTIME_DIR/$APP.lock")
    fi
    local lock pid i
    for lock in "${locks[@]}"; do
        pid=$(instance_pid "$lock")
        [ -n "$pid" ] || continue
        say "Stopping running instance (PID $pid)"
        kill -TERM "$pid" 2>/dev/null || continue
        for ((i = 0; i < 50; i++)); do
            [ -e "/proc/$pid" ] || break
            sleep 0.1
        done
        if [ -e "/proc/$pid" ]; then
            warn "PID $pid did not exit; killing it"
            kill -KILL "$pid" 2>/dev/null || true
        fi
    done
}

service_active() {
    user_systemctl is-active --quiet "$UNIT" 2>/dev/null
}

refresh_caches() {
    if command -v update-desktop-database >/dev/null; then
        update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null && [ -f "$ICON_ROOT/index.theme" ]; then
        gtk-update-icon-cache -q -t "$ICON_ROOT" 2>/dev/null || true
    fi
}

# Copies $1 to $2 with mode $3 through a temporary file, so a running binary is replaced safely.
put_file() {
    local src=$1 dest=$2 mode=$3
    mkdir -p "$(dirname "$dest")"
    install -m "$mode" "$src" "$dest.new"
    mv -f "$dest.new" "$dest"
}

do_install() {
    local src=$1 autostart=$2

    if [ -z "$autostart" ] && [ -r "$INSTALL_ARGS" ]; then
        local line
        while read -r line; do
            case $line in
                --autostart) autostart=yes ;;
                --no-autostart) autostart=no ;;
            esac
        done <"$INSTALL_ARGS"
    fi
    autostart=${autostart:-no}

    local was_active=no
    if service_active; then
        was_active=yes
    else
        stop_manual_instance
    fi

    say "Installing $APP $(payload_version)"
    put_file "$src/$APP" "$BIN_DIR/$APP" 0755
    put_file "$src/$UNIT" "$UNIT_DIR/$UNIT" 0644

    # The launcher runs from SteamVR, which does not have ~/.local/bin on PATH, so Exec= must be
    # absolute. Replace the payload's @EXEC@ token when it has one and normalise any Exec= line.
    local exec_path=$BIN_DIR/$APP desktop line out=""
    if [[ $exec_path == *[[:space:]]* ]]; then
        exec_path="\"$exec_path\""
    fi
    desktop=$(<"$src/$APP.desktop")
    desktop=${desktop//@EXEC@/$exec_path}
    while IFS= read -r line; do
        case $line in
            Exec=*) line="Exec=$exec_path" ;;
        esac
        out+=$line$'\n'
    done <<<"$desktop"
    mkdir -p "$APPS_DIR"
    printf '%s' "$out" >"$APPS_DIR/$APP.desktop.new"
    chmod 0644 "$APPS_DIR/$APP.desktop.new"
    mv -f "$APPS_DIR/$APP.desktop.new" "$APPS_DIR/$APP.desktop"

    local n
    for n in "${ICON_SIZES[@]}"; do
        put_file "$src/icons/$n.png" "$ICON_ROOT/${n}x$n/apps/$APP.png" 0644
    done
    refresh_caches

    if [ ! -d "$CONFIG_DIR" ]; then
        mkdir -p "$(dirname "$CONFIG_DIR")"
        mkdir -m 0700 "$CONFIG_DIR"
    fi
    if [ "$autostart" = yes ]; then
        printf '%s\n' --autostart >"$INSTALL_ARGS"
    else
        printf '%s\n' --no-autostart >"$INSTALL_ARGS"
    fi

    if ! user_systemctl daemon-reload; then
        warn "systemctl --user is not available; autostart and restart were skipped"
        say "Installed. Start it from the SteamVR dashboard: + -> Vocal Pitch"
        return 0
    fi

    if [ "$autostart" = yes ]; then
        user_systemctl enable --quiet "$UNIT" || warn "cannot enable $UNIT"
    elif user_systemctl is-enabled --quiet "$UNIT" 2>/dev/null; then
        user_systemctl disable --quiet "$UNIT" || warn "cannot disable $UNIT"
    fi

    if [ "$was_active" = yes ]; then
        say "Restarting $UNIT"
        user_systemctl restart "$UNIT" || warn "cannot restart $UNIT"
    elif [ "$autostart" = yes ] && user_systemctl is-active --quiet steamvr.service 2>/dev/null; then
        say "Starting $UNIT (SteamVR is running)"
        user_systemctl start "$UNIT" || warn "cannot start $UNIT"
    fi

    say "Installed $BIN_DIR/$APP"
    if [ "$autostart" = yes ]; then
        say "Autostart: on (starts with SteamVR)"
    else
        say "Autostart: off (re-run with --autostart to enable)"
    fi
    say "Open it from the SteamVR dashboard: + -> Vocal Pitch"
}

do_uninstall() {
    local purge=$1
    if user_systemctl daemon-reload 2>/dev/null; then
        user_systemctl disable --now --quiet "$UNIT" 2>/dev/null || true
    else
        warn "systemctl --user is not available; the unit was not stopped or disabled"
    fi
    stop_manual_instance

    rm -f "$BIN_DIR/$APP" "$UNIT_DIR/$UNIT" "$APPS_DIR/$APP.desktop" "$INSTALL_ARGS"
    local n
    for n in "${ICON_SIZES[@]}"; do
        rm -f "$ICON_ROOT/${n}x$n/apps/$APP.png"
    done
    user_systemctl daemon-reload 2>/dev/null || true
    refresh_caches

    if [ "$purge" = yes ]; then
        rm -rf "$CONFIG_DIR"
        say "Removed $APP and its configuration"
    else
        say "Removed $APP (configuration kept in $CONFIG_DIR)"
    fi
}

main() {
    local autostart="" uninstall=no purge=no version=no arg
    for arg in "$@"; do
        case $arg in
            --autostart) autostart=yes ;;
            --no-autostart) autostart=no ;;
            --uninstall) uninstall=yes ;;
            --purge) purge=yes ;;
            --version) version=yes ;;
            -h | --help)
                usage
                return 0
                ;;
            *)
                usage >&2
                die "unknown option: $arg"
                ;;
        esac
    done
    if [ "$purge" = yes ] && [ "$uninstall" = no ]; then
        die "--purge only works together with --uninstall"
    fi
    if [ "$uninstall" = yes ] && [ -n "$autostart" ]; then
        die "--uninstall cannot be combined with --autostart/--no-autostart"
    fi

    local src
    src=$(payload_dir)
    if [ -z "$src" ]; then
        download_and_run "$@"
    fi

    # --version only reports the payload; it changes nothing, so it is not arch-gated.
    if [ "$version" = yes ]; then
        printf '%s %s\n' "$APP" "$(payload_version)"
        return 0
    fi

    check_environment

    if [ "$uninstall" = yes ]; then
        do_uninstall "$purge"
        return 0
    fi

    do_install "$src" "$autostart"
}

# Everything above only defines functions, so a truncated download cannot run half a script.
main "$@"
