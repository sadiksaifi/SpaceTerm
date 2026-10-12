#!/usr/bin/env bash
# Exercise actual systemd restart and durable desired-state handling in the guest.
set -euo pipefail
if [[ $(id -u) != 0 || $# != 0 ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
service=spaceterm-issue520-linux-evidence.service
installation=/opt/spaceterm-issue520-linux-evidence
registration=/etc/spaceterm-issue520-linux-evidence
scripts=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
fail() {
    echo "scope=linux-launcher-prototype classification=assertion-failed check=$1" >&2
    exit 1
}
pass() { echo "scope=linux-launcher-prototype case=$1 result=passed"; }
bash "$scripts/verify.sh"
pid=$(systemctl show "$service" -p MainPID --value)
kill -KILL "$pid"
for _ in {1..30}; do
    next=$(systemctl show "$service" -p MainPID --value)
    if [[ $next != 0 && $next != "$pid" ]]; then break; fi
    sleep 1
done
[[ $next != 0 && $next != "$pid" ]] || fail crash-restart
sleep 1
bash "$scripts/verify.sh"
pass crash-restart
"$installation/manage" stop
sleep 4
[[ $(systemctl show "$service" -p MainPID --value) == 0 ]] || fail stopped-restart
[[ $(systemctl is-enabled "$service") == disabled ]] || fail stopped-supervision
grep -Fxq state=stopped "$registration/registration" || fail stopped-record
pass stop-disables-restart
# Stopped must be read before retaining any installation. There is no server
# ELF at its fixed lookup path during this test.
mv "$installation/server" "$installation/server.hidden"
trap 'if [[ -f $installation/server.hidden ]]; then mv "$installation/server.hidden" "$installation/server"; fi; rm -f -- "$registration/registration.next"' EXIT
set +e
"$installation/launcher"
result=$?
set -e
[[ $result == 78 ]] || fail stopped-before-retention
mv "$installation/server.hidden" "$installation/server"
pass stopped-before-retention
# External service activation must not bypass the durable registration guard.
systemctl start "$service"
sleep 4
[[ $(systemctl show "$service" -p MainPID --value) == 0 ]] || fail stopped-manual-start
[[ $(systemctl show "$service" -p ExecMainStatus --value) == 78 ]] || fail stopped-exit
[[ $(systemctl show "$service" -p NRestarts --value) == 0 ]] || fail stopped-restart-loop
pass stopped-manual-start
before=$(sha256sum "$registration/registration")
printf 'incomplete\n' >"$registration/registration.next"
if "$installation/control" running; then fail uncertain-publication; fi
[[ $(sha256sum "$registration/registration") == "$before" ]] || fail uncertain-record-change
rm "$registration/registration.next"
pass incomplete-publication-rejected
"$installation/manage" start
sleep 2
systemctl is-enabled --quiet "$service" || fail start-supervision
bash "$scripts/verify.sh"
pass explicit-start-restores-restart
