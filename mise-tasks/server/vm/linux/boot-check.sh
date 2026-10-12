#!/usr/bin/env bash
# Run after an externally requested guest reboot. This script never reboots.
set -euo pipefail
if [[ $(id -u) != 0 || $# != 1 || ($1 != running && $1 != stopped) ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
service=spaceterm-issue520-linux-evidence.service
registration=/etc/spaceterm-issue520-linux-evidence/registration
fail() {
    echo "scope=linux-launcher-prototype classification=assertion-failed check=$1" >&2
    exit 1
}
grep -Fxq "state=$1" "$registration" || fail boot-registration
if [[ $1 == running ]]; then
    scripts=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    bash "$scripts/verify.sh"
    invocation=$(systemctl show "$service" -p InvocationID --value)
    journalctl --quiet -b --no-pager -o cat "_SYSTEMD_INVOCATION_ID=$invocation" \
        | grep -Fxq 'scope=linux-launcher-prototype login_sessions_at_entry=0' || fail boot-login-independence
    echo 'scope=linux-launcher-prototype case=boot-before-login result=passed login_sessions_at_entry=0'
else
    [[ $(systemctl show "$service" -p MainPID --value) == 0 ]] || fail boot-process
    [[ $(systemctl is-enabled "$service") == disabled ]] || fail boot-supervision
    if journalctl --quiet -b --no-pager -o cat -u "$service" | grep -Fq ready=1; then
        fail boot-started
    fi
    echo 'scope=linux-launcher-prototype case=stopped-through-reboot result=passed process=absent supervision=disabled'
fi
printf 'scope=linux-launcher-prototype boot_id=%s\n' "$(cat /proc/sys/kernel/random/boot_id)"
