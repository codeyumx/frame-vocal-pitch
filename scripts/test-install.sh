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
    pkill -f "$T/home" 2>/dev/null
    pkill -f "$T/stub" 2>/dev/null
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
# Every path, type and mode under $1 plus every file's sha256: equal output = byte-identical tree.
snapshot() {
    (cd "$1" && find . -printf '%m %y %p\n' | sort && find . -type f -exec sha256sum {} + | sort)
}
# Last non-empty line of $1.
last_line() { printf '%s\n' "$1" | awk 'NF { l = $0 } END { print l }'; }

mkdir -p "$T"/{bin,stub,stub2,dist,dist-bad,payload,lone,run,releases}

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
# "stop a manually started instance" path can be observed. MARK makes builds for different
# versions differ byte for byte.
cat >"$T/stub/stub.c" <<'EOF'
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
static const char mark[] = MARK;
int main(void)
{
    const char *lock = getenv("FVP_TEST_LOCK");
    if (getenv("FVP_TEST_MARK"))
        puts(mark);
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
cc -O1 -DMARK="\"$VERSION\"" -o "$T/stub/frame-vocal-pitch" "$T/stub/stub.c" || exit 1

hr "package a $VERSION payload around the stub binary"
FVP_PACKAGE_ALLOW_ARCH=1 DIST_DIR="$T/dist" \
    "$ROOT/scripts/package.sh" "$VERSION" "$T/stub/frame-vocal-pitch" || exit 1
tar -xzf "$T/dist/frame-vocal-pitch-$VERSION-aarch64.tar.gz" -C "$T/payload"
PAYLOAD=$T/payload/frame-vocal-pitch-$VERSION

# Releases for the --release tests, laid out like releases/download/<tag>/ on GitHub:
#   v9.9.9   the payload above          v9.10.0  a newer, different binary
#   v9.11.0  SHA256SUMS does not match  v9.12.0  no SHA256SUMS
#   v9.13.0  SHA256SUMS lists another version's tarball           (v9.99.0 does not exist)
NEW=9.10.0
cc -O1 -DMARK="\"$NEW\"" -o "$T/stub2/frame-vocal-pitch" "$T/stub/stub.c" || exit 1
cp -r "$T/dist" "$T/releases/v$VERSION"
for v in "$NEW" 9.11.0 9.12.0; do
    FVP_PACKAGE_ALLOW_ARCH=1 DIST_DIR="$T/releases/v$v" \
        "$ROOT/scripts/package.sh" "$v" "$T/stub2/frame-vocal-pitch" >/dev/null || exit 1
done
printf '%064d  frame-vocal-pitch-9.11.0-aarch64.tar.gz\n' 0 >"$T/releases/v9.11.0/SHA256SUMS"
rm "$T/releases/v9.12.0/SHA256SUMS"
mkdir -p "$T/releases/v9.13.0"
cp "$T/releases/v$NEW/"* "$T/releases/v9.13.0/"

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
UPDATER=$H/.local/share/frame-vocal-pitch/install.sh
cmp -s "$PAYLOAD/install.sh" "$UPDATER" &&
    ok "updater copy installed at ~/.local/share/frame-vocal-pitch/install.sh" ||
    bad "updater copy missing or differs from the payload's install.sh"
mode=$(stat -c %a "$UPDATER" 2>/dev/null)
[ "$mode" = 755 ] && ok "updater copy is 0755" || bad "updater copy is ${mode:-missing}"
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

hr "7. --uninstall keeps the configuration, removes the updater and the cache"
mkdir -p "$H/.cache/frame-vocal-pitch"
: >"$H/.cache/frame-vocal-pitch/update.log"
: >"$H/.cache/frame-vocal-pitch/update-check.json"
HOME=$H FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --uninstall
expect_gone "$H/.local/share/frame-vocal-pitch"
expect_gone "$H/.cache/frame-vocal-pitch"
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

hr "12. self-update: the installed updater with --release"
H5=$T/home5
UPD5=$H5/.local/share/frame-vocal-pitch/install.sh
mkdir -p "$H5"
HOME=$H5 FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --autostart >/dev/null
expect_there "$UPD5"

# A running instance must survive every refused update.
rm -f "$T/run/frame-vocal-pitch.lock"
HOME=$H5 FVP_TEST_LOCK=$T/run/frame-vocal-pitch.lock "$H5/.local/bin/frame-vocal-pitch" &
running=$!
for _ in $(seq 50); do
    [ -s "$T/run/frame-vocal-pitch.lock" ] && break
    sleep 0.1
done

# Runs the installed updater like the app does: lone file, output appended to one log.
run_updater() {
    : >"$T/update.log"
    HOME=$H5 FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
        FVP_INSTALL_DOWNLOAD_URL="file://$T/releases" \
        /bin/bash "$UPD5" "$@" >>"$T/update.log" 2>&1
}

# refuse NAME PATTERN ARGS...: the run fails, its last line matches PATTERN, and nothing changed.
refuse() {
    local what=$1 pattern=$2 rc last before after
    shift 2
    before=$(snapshot "$H5")
    : >"$FVP_TEST_SYSTEMCTL_LOG"
    run_updater "$@"
    rc=$?
    after=$(snapshot "$H5")
    last=$(last_line "$(cat "$T/update.log")")
    printf '  %s -> exit %s, last line: %s\n' "$what" "$rc" "$last"
    [ $rc -ne 0 ] && ok "$what: refused" || bad "$what: exit 0"
    [[ $last == *$pattern* ]] && ok "$what: last line names the reason" ||
        bad "$what: last line does not mention '$pattern'"
    if [ "$before" = "$after" ]; then
        ok "$what: installation byte-identical"
    else
        bad "$what: installation changed"
        diff <(echo "$before") <(echo "$after")
    fi
    [ -s "$FVP_TEST_SYSTEMCTL_LOG" ] && bad "$what: systemctl was called" ||
        ok "$what: no systemctl call"
}

refuse "missing tag" "release v9.99.0 or its SHA256SUMS not found" --release v9.99.0
refuse "checksum mismatch" "checksum mismatch" --release v9.11.0
refuse "missing SHA256SUMS" "release v9.12.0 or its SHA256SUMS not found" --release v9.12.0
refuse "SHA256SUMS of another version" "lists no frame-vocal-pitch-9.13.0-aarch64.tar.gz" \
    --release v9.13.0
refuse "malformed tag" "needs a tag such as v1.2.3" --release 9.10.0
refuse "tag missing after --release" "needs a tag such as v1.2.3" --release
refuse "--release with --uninstall" "cannot be combined with --release" --uninstall --release v9.10.0

if kill -0 "$running" 2>/dev/null; then
    ok "the running instance survived the refused updates"
else
    bad "the running instance was stopped by a refused update"
fi

out=$(HOME=$H5 FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    bash "$PAYLOAD/install.sh" --release "v$NEW" 2>&1)
rc=$?
echo "  from a release folder -> exit $rc, last line: $(last_line "$out")"
[ $rc -ne 0 ] && [[ $(last_line "$out") == *"only works when install.sh is piped"* ]] &&
    ok "--release is refused inside an extracted release" || bad "--release accepted in a release folder"

out=$(HOME=$H5 FVP_INSTALL_ALLOW_ARCH=1 XDG_RUNTIME_DIR=$T/run \
    FVP_INSTALL_DOWNLOAD_URL="file://$T/releases" \
    bash -s -- --version --release "v$NEW" <"$T/lone/install.sh")
printf '%s\n' "$out" | grep -qx "frame-vocal-pitch $NEW" &&
    ok "piped --version --release v$NEW prints $NEW" || bad "piped --version --release wrong"

: >"$FVP_TEST_SYSTEMCTL_LOG"
run_updater --release "v$NEW"
rc=$?
echo "-- update.log"
cat "$T/update.log"
[ $rc -eq 0 ] && ok "update to v$NEW exit 0" || bad "update to v$NEW exit $rc"
cmp -s "$T/stub2/frame-vocal-pitch" "$H5/.local/bin/frame-vocal-pitch" &&
    ok "binary is the v$NEW build" || bad "binary is not the v$NEW build"
grep -qx "PAYLOAD_VERSION='$NEW'" "$UPD5" &&
    ok "updater copy replaced by the v$NEW installer" || bad "updater copy not updated"
[ "$(cat "$H5/.config/frame-vocal-pitch/install-args")" = --autostart ] &&
    ok "install-args still just --autostart (no --release recorded)" ||
    bad "install-args: $(tr '\n' ' ' <"$H5/.config/frame-vocal-pitch/install-args")"
grep -q "systemctl --user enable" "$FVP_TEST_SYSTEMCTL_LOG" &&
    ok "recorded --autostart applied by the update" || bad "autostart lost in the update"
if kill -0 "$running" 2>/dev/null; then
    bad "manually started instance still running after the update"
    kill -KILL "$running" 2>/dev/null
else
    ok "manually started instance stopped by the update"
fi
wait "$running" 2>/dev/null

run_updater --release "v$VERSION"
rc=$?
[ $rc -eq 0 ] && cmp -s "$T/stub/frame-vocal-pitch" "$H5/.local/bin/frame-vocal-pitch" &&
    grep -qx "PAYLOAD_VERSION='$VERSION'" "$UPD5" &&
    ok "--release v$VERSION installs exactly that (older) version" ||
    bad "--release v$VERSION did not install $VERSION (exit $rc)"

printf '\n'
if [ $FAILED -eq 0 ]; then
    printf '%sALL CHECKS PASSED%s\n' "$C_OK" "$C_OFF"
else
    printf '%sFAILURES PRESENT%s\n' "$C_BAD" "$C_OFF"
fi
exit $FAILED
