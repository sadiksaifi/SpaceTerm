#!/usr/bin/env bash
# Install an unauthenticated, credential-free test fixture in a disposable VM only.
set -euo pipefail
if [[ ${1:-} != --disposable-jammy || $(id -u) != 0 || $# -gt 2 ||
($# == 2 && $2 != --refresh-prototype) ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
# shellcheck source=/dev/null
source /etc/os-release
if [[ $ID != ubuntu || $VERSION_ID != 22.04 || $(uname -m) != x86_64 ]]; then
    echo 'classification=unsupported-guest' >&2
    exit 2
fi
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../../.." && pwd)
source_dir=$root/tests/server_native/linux
installation=/opt/spaceterm-issue520-linux-evidence
registration=/etc/spaceterm-issue520-linux-evidence
refresh=false
if [[ -e $installation || -e $registration ]]; then
    if [[ ${2:-} != --refresh-prototype || ! -f $registration/registration ||
        -L $installation || -L $registration ]]; then
        echo 'classification=fixture-already-installed' >&2
        exit 2
    fi
    refresh=true
fi
build=$(mktemp -d)
trap 'rm -rf -- "$build"' EXIT
flags=(-std=c11 -O2 -Wall -Wextra -Werror -pedantic)
for program in launcher control; do
    cc "${flags[@]}" "$source_dir/registration.c" "$source_dir/$program.c" -o "$build/$program"
done
for program in server ptrace_probe mount_evidence; do
    cc "${flags[@]}" "$source_dir/$program.c" -o "$build/$program"
done
if $refresh; then
    systemctl stop spaceterm-issue520-linux-evidence.service
fi
install -d -o root -g root -m 0755 "$installation"
install -d -o root -g root -m 0700 "$registration"
for program in launcher control mount_evidence; do
    install -o root -g root -m 0700 "$build/$program" "$installation/$program"
done
install -o root -g root -m 0755 "$build/ptrace_probe" "$installation/ptrace-probe"
# Main ELF is executable but unreadable to the final account. No set-ID bits.
install -o root -g root -m 0501 "$build/server" "$installation/server"
install -o root -g root -m 0700 "$root/mise-tasks/server/vm/linux/manage.sh" "$installation/manage"
if ! $refresh; then
    printf 'STLN1\nuid=%s\ngid=%s\nstate=stopped\n' "$(id -u sdk)" "$(id -g sdk)" >"$registration/registration"
    chmod 0600 "$registration/registration"
    install -o root -g root -m 0600 /dev/null "$registration/lock"
    install -o root -g root -m 0600 /dev/null "$registration/operations.lock"
    sync -f "$registration/registration"
    sync -f "$registration"
fi
install -o root -g root -m 0644 "$source_dir/spaceterm-issue520-linux-evidence.service" \
    /etc/systemd/system/spaceterm-issue520-linux-evidence.service
printf 'fs.suid_dumpable=0\n' >/etc/sysctl.d/99-spaceterm-issue520-linux-evidence.conf
chmod 0644 /etc/sysctl.d/99-spaceterm-issue520-linux-evidence.conf
# Stock Jammy apport writes fs.suid_dumpable=2 after systemd-sysctl at boot.
# Mask it only in this disposable fixture. A production custody profile must
# independently establish this policy and account for native crash collectors.
systemctl mask --now apport.service
sysctl -w fs.suid_dumpable=0
systemd-analyze verify /etc/systemd/system/spaceterm-issue520-linux-evidence.service
systemctl daemon-reload
if ! $refresh; then
    "$installation/manage" start
elif grep -Fxq state=running "$registration/registration"; then
    systemctl start spaceterm-issue520-linux-evidence.service
fi
"$installation/mount_evidence"
