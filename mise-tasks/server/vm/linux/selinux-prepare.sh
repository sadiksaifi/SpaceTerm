#!/usr/bin/env bash
# Provision SELinux permissively first. Never applies to the penguin host.
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
env DEBIAN_FRONTEND=noninteractive apt-get update
env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    selinux-basics selinux-policy-default selinux-policy-dev policycoreutils \
    policycoreutils-python-utils selinux-utils checkpolicy setools auditd
# First boot must stay permissive while the stock policy relabels the guest.
sed -i 's/^SELINUX=.*/SELINUX=permissive/' /etc/selinux/config
selinux-activate
cat >/etc/default/grub.d/99-spaceterm-issue520-linux-evidence.cfg <<'GRUB'
GRUB_CMDLINE_LINUX="$GRUB_CMDLINE_LINUX security=selinux selinux=1 enforcing=0"
GRUB
update-grub
touch /.autorelabel
dpkg-query -W -f='${Package}=${Version}\n' selinux-policy-default selinux-basics checkpolicy
printf 'scope=linux-selinux-prototype classification=reboot-required enforcement=not-established\n'
