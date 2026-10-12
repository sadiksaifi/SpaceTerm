#!/usr/bin/env bash
# Exercise the verifier's cleanup through its public script in a private mount namespace.
set -euo pipefail
if [[ $(id -u) != 0 || $# -gt 1 ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
# shellcheck source=/dev/null
source /etc/os-release
if [[ $ID != ubuntu || $VERSION_ID != 22.04 || $(uname -r) != 5.15.* ]]; then
    echo 'classification=unsupported-guest' >&2
    exit 2
fi
if [[ $# == 0 ]]; then
    exec unshare --mount --propagation private bash "${BASH_SOURCE[0]}" --isolated
fi
if [[ $1 != --isolated || $(readlink /proc/self/ns/mnt) == "$(readlink /proc/1/ns/mnt)" ||
$(findmnt -n -o PROPAGATION /) != private ]]; then
    echo 'classification=namespace-unavailable' >&2
    exit 2
fi
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
verifier=$root/mise-tasks/server/vm/linux/selinux-verify.sh
installation=/opt/spaceterm-issue520-linux-evidence
registration=/etc/spaceterm-issue520-linux-evidence
control=/var/tmp/spaceterm-issue520-linux-evidence-control
alias=/mnt/spaceterm-issue520-linux-evidence-alias
previous=$(getenforce)
[[ $previous != Disabled ]] || exit 2
build=$(mktemp -d)
stop_holder() {
    if [[ -f $build/holder.pid ]]; then
        local pid
        pid=$(cat "$build/holder.pid")
        kill "$pid" || return 1
        for _ in {1..100}; do
            if [[ ! -e /proc/$pid/cwd ]]; then break; fi
            sleep 0.01
        done
        [[ ! -e /proc/$pid/cwd ]] || return 1
        rm -f -- "$build/holder.pid" "$build/holder.ready"
    fi
}
cleanup() {
    local status=$? cleanup_failed=0
    trap - EXIT
    if ! setenforce "$previous"; then cleanup_failed=1; fi
    if ! stop_holder; then cleanup_failed=1; fi
    if ! rm -rf -- "$build"; then cleanup_failed=1; fi
    if [[ $status == 0 && $cleanup_failed != 0 ]]; then status=1; fi
    exit "$status"
}
trap cleanup EXIT
# Overlays keep the live service and any interrupted experiment's data untouched.
mkdir "$build/installation" "$build/registration" "$build/var-tmp" "$build/mnt" "$build/bin"
cp -a "$installation/." "$build/installation/"
mount --bind "$build/installation" "$installation"
mount --bind "$build/registration" "$registration"
mount --bind "$build/var-tmp" /var/tmp
mount --bind "$build/mnt" /mnt
restorecon -RF "$installation" "$registration"
restorecon /var/tmp /mnt
chmod 0700 "$registration"
# Failure injection stays at external commands. The verifier and native probes remain real.
cat >"$build/bin/umount" <<'WRAPPER'
#!/usr/bin/env bash
set -euo pipefail
if [[ $# == 1 && $1 == /mnt/spaceterm-issue520-linux-evidence-alias ]]; then
    if [[ ${STLN_TEST_UNMOUNT_FAILURE:-0} == 1 ]]; then
        echo 'classification=injected-unmount-failure' >&2
        exit 32
    fi
    if [[ ${STLN_TEST_BUSY_ALIAS:-0} == 1 ]]; then
        bash -c 'cd -- "$1"; : >"$2"; exec sleep 60' _ "$1" "$STLN_TEST_HOLDER.ready" >/dev/null 2>&1 &
        echo "$!" >"$STLN_TEST_HOLDER.pid"
        for _ in {1..100}; do
            if [[ -f $STLN_TEST_HOLDER.ready ]]; then break; fi
            sleep 0.01
        done
        [[ -f $STLN_TEST_HOLDER.ready ]]
    fi
fi
exec /usr/bin/umount "$@"
WRAPPER
cat >"$build/bin/setenforce" <<'WRAPPER'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STLN_TEST_ENFORCEMENT_LOG"
exec /usr/sbin/setenforce "$@"
WRAPPER
cat >"$build/bin/rm" <<'WRAPPER'
#!/usr/bin/env bash
if [[ ${STLN_TEST_REMOVE_FAILURE:-0} == 1 && ${!#} == /opt/spaceterm-issue520-linux-evidence/custody ]]; then
    exit 97
fi
exec /usr/bin/rm "$@"
WRAPPER
cat >"$build/bin/chmod" <<'WRAPPER'
#!/usr/bin/env bash
if [[ ${STLN_TEST_CHMOD_FAILURE:-0} == 1 && $1 == 700 && $2 == /etc/spaceterm-issue520-linux-evidence ]]; then
    exit 96
fi
exec /usr/bin/chmod "$@"
WRAPPER
cat >"$build/bin/runuser" <<'WRAPPER'
#!/usr/bin/env bash
if [[ ${STLN_TEST_PROBE_FAILURE:-0} == 1 && ${!#} == --unconfined-control ]]; then
    exit 17
fi
exec /usr/sbin/runuser "$@"
WRAPPER
chmod 0755 "$build/bin/"*
failures=0
cases=0
reset_fixtures() {
    stop_holder
    if mountpoint -q "$alias"; then /usr/bin/umount "$alias"; fi
    rm -rf -- "$installation/custody" "$registration/custody" "$control" "$alias"
    chmod 0700 "$registration"
    setenforce 0
}
fixtures_absent() {
    local fixture
    for fixture in "$installation/custody" "$registration/custody" "$control" "$alias"; do
        if [[ -e $fixture || -L $fixture ]]; then return 1; fi
    done
    ! mountpoint -q "$alias"
}
run_verifier() {
    : >"$build/enforcement"
    if env LC_ALL=C "PATH=$build/bin:$PATH" "STLN_TEST_ENFORCEMENT_LOG=$build/enforcement" \
        "STLN_TEST_HOLDER=$build/holder" "$@" bash "$verifier" >"$build/result" 2>&1; then
        result=0
    else
        result=$?
    fi
}
report_case() {
    cases=$((cases + 1))
    if [[ $2 == passed ]]; then
        printf 'scope=linux-selinux-cleanup case=%s result=passed\n' "$1"
    else
        failures=$((failures + 1))
        printf 'scope=linux-selinux-cleanup case=%s result=failed verifier_status=%s enforcement=%s\n' \
            "$1" "$result" "$(getenforce)" >&2
        cat "$build/result" >&2
    fi
}
for fixture in "$installation/custody" "$registration/custody" "$control" "$alias"; do
    reset_fixtures
    mkdir "$fixture"
    printf 'retained-diagnostic-data\n' >"$fixture/sentinel"
    run_verifier
    if [[ $result != 0 && -f $fixture/sentinel && ! -s $build/enforcement ]] \
        && [[ $(cat "$fixture/sentinel") == retained-diagnostic-data ]] \
        && [[ $(getenforce) == Permissive && $(stat -c %a "$registration") == 700 ]]; then
        report_case "collision-$cases" passed
    else
        report_case "collision-$cases" failed
    fi
    reset_fixtures
    ln -s "$build/absent-target" "$fixture"
    run_verifier
    if [[ $result != 0 && -L $fixture && ! -s $build/enforcement ]] \
        && [[ $(readlink "$fixture") == "$build/absent-target" && $(getenforce) == Permissive ]]; then
        report_case "dangling-symlink-$cases" passed
    else
        report_case "dangling-symlink-$cases" failed
    fi
done
reset_fixtures
mkdir "$alias" "$build/foreign"
printf 'retained-diagnostic-data\n' >"$build/foreign/sentinel"
mount --bind "$build/foreign" "$alias"
run_verifier
if [[ $result != 0 && ! -s $build/enforcement ]] && mountpoint -q "$alias" \
    && [[ $(cat "$alias/sentinel") == retained-diagnostic-data && $(getenforce) == Permissive ]]; then
    report_case foreign-mount passed
else
    report_case foreign-mount failed
fi
for state in Permissive Enforcing; do
    reset_fixtures
    chmod 0750 "$registration"
    setenforce "$state"
    run_verifier
    if [[ $result == 0 && $(getenforce) == "$state" && $(stat -c %a "$registration") == 750 ]] \
        && fixtures_absent && grep -Fq 'confined_denials=36' "$build/result"; then
        report_case "success-$state" passed
    else
        report_case "success-$state" failed
    fi
done
for fault in STLN_TEST_UNMOUNT_FAILURE STLN_TEST_BUSY_ALIAS; do
    reset_fixtures
    run_verifier "$fault=1"
    if [[ $fault == STLN_TEST_BUSY_ALIAS ]] && ! grep -Fq 'target is busy' "$build/result"; then
        report_case "$fault" failed
        continue
    fi
    if [[ $result == 1 && $(getenforce) == Permissive && $(stat -c %a "$registration") == 700 &&
    -f $installation/custody/file && ! -e $registration/custody && ! -e $control ]] \
        && mountpoint -q "$alias" && grep -Fq 'classification=cleanup-incomplete check=unmount' "$build/result"; then
        report_case "$fault" passed
    else
        report_case "$fault" failed
    fi
done
reset_fixtures
run_verifier STLN_TEST_REMOVE_FAILURE=1
if [[ $result == 1 && $(getenforce) == Permissive && $(stat -c %a "$registration") == 700 &&
-f $installation/custody/file && ! -e $registration/custody && ! -e $control && ! -e $alias ]] \
    && grep -Fq 'classification=cleanup-incomplete check=fixture-removal' "$build/result"; then
    report_case removal-failure passed
else
    report_case removal-failure failed
fi
reset_fixtures
run_verifier STLN_TEST_CHMOD_FAILURE=1
if [[ $result == 1 && $(getenforce) == Permissive && $(stat -c %a "$registration") == 755 ]] \
    && fixtures_absent && grep -Fq 'classification=cleanup-incomplete check=registration-mode' "$build/result"; then
    report_case chmod-failure passed
else
    report_case chmod-failure failed
fi
reset_fixtures
run_verifier STLN_TEST_PROBE_FAILURE=1 STLN_TEST_UNMOUNT_FAILURE=1
if [[ $result == 17 && $(getenforce) == Permissive && $(stat -c %a "$registration") == 700 ]] \
    && mountpoint -q "$alias" && grep -Fq 'classification=cleanup-incomplete check=unmount' "$build/result"; then
    report_case original-failure-preserved passed
else
    report_case original-failure-preserved failed
fi
reset_fixtures
printf 'scope=linux-selinux-cleanup cases=%s failures=%s\n' "$cases" "$failures"
[[ $failures == 0 ]]
