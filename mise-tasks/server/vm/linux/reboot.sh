#!/usr/bin/env bash
# Explicit guest-only reboot. QEMU/container lifecycle remains the coordinator's.
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
systemctl reboot
