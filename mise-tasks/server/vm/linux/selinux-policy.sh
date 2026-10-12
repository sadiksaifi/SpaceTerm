#!/usr/bin/env bash
# Install only the disposable mechanism experiment, not a production custody profile.
set -euo pipefail
if [[ ${1:-} != --disposable-jammy || $# != 1 || $(id -u) != 0 ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
# shellcheck source=/dev/null
source /etc/os-release
if [[ $ID != ubuntu || $VERSION_ID != 22.04 || $(uname -r) != 5.15.* ]]; then
    echo 'classification=unsupported-guest' >&2
    exit 2
fi
if ! selinuxenabled; then
    echo 'classification=selinux-unavailable' >&2
    exit 1
fi
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../../.." && pwd)
source_dir=$root/tests/server_native/linux
build=$(mktemp -d)
trap 'rm -rf -- "$build"' EXIT
cp "$source_dir/spaceterm_issue520_linux_evidence.te" "$source_dir/spaceterm_issue520_linux_evidence.fc" "$build/"
make -C "$build" -f /usr/share/selinux/devel/Makefile spaceterm_issue520_linux_evidence.pp
semodule -i "$build/spaceterm_issue520_linux_evidence.pp"
cc -std=c11 -O2 -Wall -Wextra -Werror -pedantic "$source_dir/custody_probe.c" \
    -lselinux -o "$build/custody-probe"
install -o root -g root -m 0755 "$build/custody-probe" /opt/spaceterm-issue520-linux-evidence/custody-probe
restorecon -RF /opt/spaceterm-issue520-linux-evidence /etc/spaceterm-issue520-linux-evidence
sestatus
ls -Zd /opt/spaceterm-issue520-linux-evidence /etc/spaceterm-issue520-linux-evidence
printf 'scope=linux-selinux-prototype module=installed production_custody=unavailable\n'
