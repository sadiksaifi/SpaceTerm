#!/usr/bin/env bash
# Observe the installed placeholder, never claim production custody or credentials.
set -euo pipefail
if [[ $(id -u) != 0 || $# != 0 ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
service=spaceterm-issue520-linux-evidence.service
installation=/opt/spaceterm-issue520-linux-evidence
fail() {
    echo "scope=linux-launcher-prototype classification=assertion-failed check=$1" >&2
    exit 1
}
systemctl is-active --quiet "$service" || fail active
pid=$(systemctl show "$service" -p MainPID --value)
[[ $pid =~ ^[1-9][0-9]*$ ]] || fail pid
uid=$(id -u sdk)
gid=$(id -g sdk)
status=$(cat "/proc/$pid/status")
grep -Eq "^Uid:[[:space:]]+${uid}[[:space:]]+${uid}[[:space:]]+${uid}[[:space:]]+$uid$" <<<"$status" || fail uid
grep -Eq "^Gid:[[:space:]]+${gid}[[:space:]]+${gid}[[:space:]]+${gid}[[:space:]]+$gid$" <<<"$status" || fail gid
grep -Eq '^Groups:[[:space:]]*$' <<<"$status" || fail groups
for field in CapInh CapPrm CapEff CapBnd CapAmb; do
    grep -Eq "^$field:[[:space:]]+0000000000000000$" <<<"$status" || fail capabilities
done
grep -Eq '^NoNewPrivs:[[:space:]]+1$' <<<"$status" || fail no-new-privs
[[ $(cat /proc/sys/fs/suid_dumpable) == 0 ]] || fail dumpability-sysctl
[[ $(stat -c %u "/proc/$pid/fd") == 0 ]] || fail sensitive-proc-owner
runuser -u sdk -- test ! -r "$installation/server" || fail executable-readable
if runuser -u sdk -- readlink "/proc/$pid/exe" >/dev/null 2>&1; then
    fail proc-exe-readable
fi
invocation=$(systemctl show "$service" -p InvocationID --value)
journalctl --quiet --no-pager -o cat "_SYSTEMD_INVOCATION_ID=$invocation" \
    | grep -Fxq 'scope=linux-launcher-prototype initial_supplementary_groups=3' || fail initial-groups
journalctl --quiet --no-pager -o cat "_SYSTEMD_INVOCATION_ID=$invocation" \
    | grep -Fq "scope=linux-launcher-prototype uid=$uid gid=$gid groups=0 dumpable=0 no_new_privs=1 ready=1" || fail post-exec-dumpability
cgroup=$(systemctl show "$service" -p ControlGroup --value)
[[ $cgroup == /system.slice/spaceterm-issue520-linux-evidence.service ]] || fail cgroup
[[ $(cat "/sys/fs/cgroup$cgroup/cgroup.procs") == "$pid" ]] || fail root-or-child-process
[[ $(find "/proc/$pid/fd" -mindepth 1 -maxdepth 1 | wc -l) == 3 ]] || fail descriptors
listeners=$(ss -Hlntupx)
if grep -Fq "pid=$pid," <<<"$listeners"; then
    fail listener
fi
# Disable Yama only for this disposable test. Otherwise EPERM could be Yama,
# rather than the unreadable ELF's exec-time nondumpability. Restore on exit.
yama=$(cat /proc/sys/kernel/yama/ptrace_scope)
trap 'sysctl -q -w "kernel.yama.ptrace_scope=$yama"' EXIT
sysctl -q -w kernel.yama.ptrace_scope=0
runuser -u sdk -- "$installation/ptrace-probe" "$pid"
printf 'scope=linux-launcher-prototype case=final-state result=passed uid=%s gid=%s supplementary_groups=0 capabilities=0 dumpable=0 no_new_privs=1 descriptors=3 processes=1 listeners=0\n' "$uid" "$gid"
