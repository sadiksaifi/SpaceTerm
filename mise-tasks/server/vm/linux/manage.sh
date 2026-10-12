#!/usr/bin/env bash
# Test composition only. No production installation or authorization authority.
set -euo pipefail
if [[ $(id -u) != 0 || $# != 1 || ($1 != start && $1 != stop) ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
service=spaceterm-issue520-linux-evidence.service
installation=/opt/spaceterm-issue520-linux-evidence
registration=/etc/spaceterm-issue520-linux-evidence
# Serialize the desired-state publication with derived systemd configuration.
exec 9<>"$registration/operations.lock"
flock -x 9
if [[ $1 == stop ]]; then
    "$installation/control" stopped
    systemctl disable --now "$service"
    if [[ $(systemctl show "$service" -p MainPID --value) != 0 ]]; then
        echo 'classification=stop-incomplete' >&2
        exit 1
    fi
    echo 'scope=linux-launcher-prototype desired_state=stopped supervision=disabled process=absent'
else
    "$installation/control" running
    systemctl enable --now "$service"
    echo 'scope=linux-launcher-prototype desired_state=running supervision=enabled'
fi
