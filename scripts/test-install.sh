#!/usr/bin/env bash
# Exercises packaging/install.sh end to end without touching the real installation:
# throwaway HOME directories, a fake payload packaged by scripts/package.sh, a stub systemctl
# that records the calls it receives, and FVP_INSTALL_ALLOW_ARCH=1 for the aarch64 check.
#
#   scripts/test-install.sh
#
# Needs cc (for a stub binary that the installer can start and stop), tar, curl and systemd-free
# user tooling. Everything it writes lives in a temporary directory that is removed on exit.
# The payload does not have to be aarch64: package.sh's arch check is relaxed for the stub.
#
# shellcheck disable=SC2015 # ok/bad always return 0, so `cond && ok … || bad …` is a plain if
# shellcheck disable=SC2329 # cleanup() runs from the EXIT trap
set -u -o pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/fvp-install-test.XXXXXX")
FAILED=0
VERSION=9.9.9

if [ -t 1 ]; then
    C_OK=$'\033[32m' C_BAD=$'\033[31m' C_HEAD=$'\033[1m' C_OFF=$'\033[0m'
else
    C_OK='' C_BAD='' C_HEAD='' C_OFF=''
fi

cleanup() {
    pkill -f "$T/home/.local/bin/frame-vocal-pitch" 2>/dev/null
    pkill -f "$T/stub/frame-vocal-pitch" 2>/dev/null
    rm -rf "$T"
}
trap cleanup EXIT

ok() { printf '  %sOK%s   %s\n' "$C_OK" "$C_OFF" "$*"; }
bad() {
    printf '  %sFAIL%s %s\n' "$C_BAD" "$C_OFF" "$*"
    FAILED=1
}
expect_gone() { if [ -e "$1" ]; then bad "still exists: $1"; else ok "removed: $1"; fi; }
expect_there() { if [ -e "$1" ]; then ok "exists: $1"; else bad "missing: $1"; fi; }
hr() { printf '\n%s== %s%s\n' "$C_HEAD" "$*" "$C_OFF"; }

mkdir -p "$T"/{bin,stub,dist,dist-bad,payload,lone,run}

# Fake systemctl: records every call with the XDG_RUNTIME_DIR it was given, and fakes the two
# state queries through marker files.
cat >"$T/bin/systemctl" <<'EOF'
#!/bin/sh
printf 'systemctl %s | XDG_RUNTIME_DIR=%s\n' "$*" "$XDG_RUNTIME_DIR" >>"$FVP_TEST_SYSTEMCTL_LOG"
for a in "$@"; do
    case $a in
        is-active)
            [ -n "${FVP_TEST_ACTIVE:-}" ] && [ -f "${FVP_TEST_ACTIVE:-}" ] && exit 0
            exit 3
            ;;
        is-enabled)
            [ -n "${FVP_TEST_ENABLED:-}" ] && [ -f "${FVP_TEST_ENABLED:-}" ] && exit 0
            exit 1
            ;;
    esac
done
exit 0
EOF
chmod +x "$T/bin/systemctl"

# Stub payload: a native binary that writes its PID to the lock file and waits for SIGTERM, so the
# "stop a manually started instance" path can be observed.
cat >"$T/stub/stub.c" <<'EOF'
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(void)
{
    const char *lock = getenv("FVP_TEST_LOCK");
    if (lock) {
        FILE *f = fopen(lock, "w");
        if (f) {
            fprintf(f, "%d\n", (int)getpid());
            fclose(f);
        }
    }
    for (;;)
        pause();
}
EOF
command -v cc >/dev/null || {
    printf 'error: cc is required to build the stub binary\n' >&2
    exit 1
}
cc -O1 -o "$T/stub/frame-vocal-pitch" "$T/stub/stub.c" || exit 1

hr "package a $VERSION payload around the stub binary"
FVP_PACKAGE_ALLOW_ARCH=1 DIST_DIR="$T/dist" \
    "$ROOT/scripts/package.sh" "$VERSION" "$T/stub/frame-vocal-pitch" || exit 1
tar -xzf "$T/dist/frame-vocal-pitch-$VERSION-aarch64.tar.gz" -C "$T/payload"
PAYLOAD=$T/payload/frame-vocal-pitch-$VERSION

export PATH="$T/bin:$PATH"
export FVP_TEST_SYSTEMCTL_LOG="$T/systemctl.log"
: >"$FVP_TEST_SYSTEMCTL_LOG"

hr "1. install with --no-autostart"
H=$T/home
mkdir -p "$H"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --no-autostart
rc=$?
[ $rc -eq 0 ] && ok "exit 0" || bad "exit $rc"

echo "-- installed tree"
find "$H" -type f | sort
echo "-- desktop entry"
cat "$H/.local/share/applications/frame-vocal-pitch.desktop"

expect_there "$H/.local/bin/frame-vocal-pitch"
[ -x "$H/.local/bin/frame-vocal-pitch" ] && ok "binary is executable" || bad "binary not executable"
cmp -s "$T/stub/frame-vocal-pitch" "$H/.local/bin/frame-vocal-pitch" &&
    ok "binary matches payload" || bad "binary differs"
cmp -s "$PAYLOAD/frame-vocal-pitch.service" "$H/.config/systemd/user/frame-vocal-pitch.service" &&
    ok "unit installed" || bad "unit differs"
for n in 48 128 256; do
    cmp -s "$PAYLOAD/icons/$n.png" "$H/.local/share/icons/hicolor/${n}x$n/apps/frame-vocal-pitch.png" &&
        ok "icon ${n}px installed" || bad "icon ${n}px differs"
done
grep -qx "Exec=$H/.local/bin/frame-vocal-pitch" "$H/.local/share/applications/frame-vocal-pitch.desktop" &&
    ok "Exec= is absolute" || bad "Exec= is not absolute"
grep -q '@EXEC@' "$H/.local/share/applications/frame-vocal-pitch.desktop" &&
    bad "@EXEC@ left in the desktop file" || ok "no @EXEC@ placeholder left"
grep -qx -- '--no-autostart' "$H/.config/frame-vocal-pitch/install-args" &&
    ok "install-args records --no-autostart" || bad "install-args wrong"
mode=$(stat -c %a "$H/.config/frame-vocal-pitch")
[ "$mode" = 700 ] && ok "config dir is 0700" || bad "config dir is $mode"
echo "-- systemctl calls"
cat "$FVP_TEST_SYSTEMCTL_LOG"
grep -q "XDG_RUNTIME_DIR=/run/user/$(id -u)" "$FVP_TEST_SYSTEMCTL_LOG" &&
    ok "systemctl got XDG_RUNTIME_DIR=/run/user/$(id -u)" || bad "XDG_RUNTIME_DIR not set for systemctl"
grep -q "systemctl --user enable" "$FVP_TEST_SYSTEMCTL_LOG" &&
    bad "enabled the unit despite --no-autostart" || ok "did not enable the unit"

hr "2. re-run is idempotent"
before=$(cd "$H" && find .local .config -type f -exec sha256sum {} + | sort)
: >"$FVP_TEST_SYSTEMCTL_LOG"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --no-autostart
after=$(cd "$H" && find .local .config -type f -exec sha256sum {} + | sort)
if [ "$before" = "$after" ]; then
    ok "installed files unchanged"
else
    bad "installed files changed"
    diff <(echo "$before") <(echo "$after")
fi

hr "3. re-run stops a manually started instance through the lock file"
HOME=$H FVP_TEST_LOCK=$T/run/frame-vocal-pitch.lock \
    "$H/.local/bin/frame-vocal-pitch" &
manual=$!
for _ in $(seq 50); do
    [ -s "$T/run/frame-vocal-pitch.lock" ] && break
    sleep 0.1
done
echo "manual instance PID $manual, lock says: $(cat "$T/run/frame-vocal-pitch.lock" 2>/dev/null)"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --no-autostart
if kill -0 "$manual" 2>/dev/null; then
    bad "manual instance still running"
    kill -KILL "$manual" 2>/dev/null
else
    ok "manual instance was stopped"
fi
wait "$manual" 2>/dev/null

hr "4. --autostart enables the unit, later runs keep it"
: >"$FVP_TEST_SYSTEMCTL_LOG"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --autostart
grep -qx -- '--autostart' "$H/.config/frame-vocal-pitch/install-args" &&
    ok "install-args records --autostart" || bad "install-args wrong"
grep -q "systemctl --user enable" "$FVP_TEST_SYSTEMCTL_LOG" &&
    ok "unit enabled" || bad "unit not enabled"
: >"$FVP_TEST_SYSTEMCTL_LOG"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh"
grep -qx -- '--autostart' "$H/.config/frame-vocal-pitch/install-args" &&
    ok "a run without flags kept --autostart" || bad "autostart choice lost"
grep -q "systemctl --user enable" "$FVP_TEST_SYSTEMCTL_LOG" &&
    ok "unit enabled again on update" || bad "unit not enabled on update"

hr "5. an active service is restarted, not started"
touch "$T/active"
: >"$FVP_TEST_SYSTEMCTL_LOG"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run FVP_TEST_ACTIVE=$T/active \
    bash "$PAYLOAD/install.sh" --no-autostart
cat "$FVP_TEST_SYSTEMCTL_LOG"
grep -q "systemctl --user restart frame-vocal-pitch.service" "$FVP_TEST_SYSTEMCTL_LOG" &&
    ok "service restarted" || bad "service not restarted"
rm -f "$T/active"

hr "6. --version (not arch-gated: it only reads the payload)"
v=$(HOME=$H bash "$PAYLOAD/install.sh" --version)
echo "$v"
[ "$v" = "frame-vocal-pitch $VERSION" ] && ok "--version prints the payload version" ||
    bad "--version printed '$v'"

hr "7. --uninstall keeps the configuration"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --uninstall
expect_gone "$H/.local/bin/frame-vocal-pitch"
expect_gone "$H/.config/systemd/user/frame-vocal-pitch.service"
expect_gone "$H/.local/share/applications/frame-vocal-pitch.desktop"
for n in 48 128 256; do expect_gone "$H/.local/share/icons/hicolor/${n}x$n/apps/frame-vocal-pitch.png"; done
expect_gone "$H/.config/frame-vocal-pitch/install-args"
expect_there "$H/.config/frame-vocal-pitch"

hr "8. --uninstall --purge removes the configuration"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --no-autostart >/dev/null
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --uninstall --purge
expect_gone "$H/.config/frame-vocal-pitch"

hr "9. piped mode: download, verify sha256, re-exec"
H2=$T/home2
mkdir -p "$H2"
cp "$ROOT/packaging/install.sh" "$T/lone/install.sh"
HOME=$H2 FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    FVP_INSTALL_DOWNLOAD_URL="file://$T/dist" \
    bash -s -- --no-autostart <"$T/lone/install.sh"
rc=$?
[ $rc -eq 0 ] && ok "piped install exit 0" || bad "piped install exit $rc"
expect_there "$H2/.local/bin/frame-vocal-pitch"
grep -qx -- '--no-autostart' "$H2/.config/frame-vocal-pitch/install-args" &&
    ok "arguments reached the re-executed installer" || bad "arguments lost in the re-exec"

hr "9b. piped --version resolves the release and prints its version"
out=$(HOME=$H2 FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    FVP_INSTALL_DOWNLOAD_URL="file://$T/dist" \
    bash -s -- --version <"$T/lone/install.sh")
printf '%s\n' "$out"
printf '%s\n' "$out" | grep -qx "frame-vocal-pitch $VERSION" &&
    ok "piped --version prints $VERSION" || bad "piped --version wrong"

hr "10. piped mode refuses a bad checksum"
cp "$T/dist/frame-vocal-pitch-$VERSION-aarch64.tar.gz" "$T/dist-bad/"
printf '%s  frame-vocal-pitch-%s-aarch64.tar.gz\n' \
    "0000000000000000000000000000000000000000000000000000000000000000" "$VERSION" \
    >"$T/dist-bad/SHA256SUMS"
H3=$T/home3
mkdir -p "$H3"
HOME=$H3 FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    FVP_INSTALL_DOWNLOAD_URL="file://$T/dist-bad" \
    bash -s -- --no-autostart <"$T/lone/install.sh"
rc=$?
[ $rc -ne 0 ] && ok "refused (exit $rc)" || bad "installed despite a bad checksum"
expect_gone "$H3/.local/bin/frame-vocal-pitch"

hr "11. refuses a non-aarch64 machine without the test escape hatch"
H4=$T/home4
mkdir -p "$H4"
HOME=$H4 XDG_RUNTIME_DIR=$T/run bash "$PAYLOAD/install.sh" --no-autostart
rc=$?
[ $rc -ne 0 ] && ok "refused to install (exit $rc)" || bad "did not refuse"
HOME=$H4 XDG_RUNTIME_DIR=$T/run bash "$PAYLOAD/install.sh" --uninstall
rc=$?
[ $rc -ne 0 ] && ok "refused to uninstall (exit $rc)" || bad "uninstall not refused"

printf '\n'
if [ $FAILED -eq 0 ]; then
    printf '%sALL CHECKS PASSED%s\n' "$C_OK" "$C_OFF"
else
    printf '%sFAILURES PRESENT%s\n' "$C_BAD" "$C_OFF"
fi
exit $FAILED
