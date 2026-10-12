#!/usr/bin/env bash
# Prove only a confined domain's inode checks, including an ordinary bind alias.
set -euo pipefail
if [[ $(id -u) != 0 || $# != 0 ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
# shellcheck source=/dev/null
source /etc/os-release
if [[ $ID != ubuntu || $VERSION_ID != 22.04 || $(uname -r) != 5.15.* ]]; then
    echo 'classification=unsupported-guest' >&2
    exit 2
fi
installation=/opt/spaceterm-issue520-linux-evidence
registration=/etc/spaceterm-issue520-linux-evidence
control=/var/tmp/spaceterm-issue520-linux-evidence-control
alias=/mnt/spaceterm-issue520-linux-evidence-alias
fail() {
    echo "scope=linux-selinux-prototype classification=assertion-failed check=$1" >&2
    exit 1
}
selinuxenabled || fail selinux-enabled
previous=$(getenforce)
[[ $previous != Disabled ]] || fail enforcement-unavailable
if mountpoint -q "$alias"; then fail foreign-alias; fi
# Refusal leaves every existing fixture untouched, including dangling symlinks.
for directory in "$installation/custody" "$registration/custody" "$control" "$alias"; do
    if [[ -e $directory || -L $directory ]]; then fail prior-fixture; fi
done
registration_mode=$(stat -c %a "$registration")
registration_mode_changed=false
owned_directories=()
alias_created=false
alias_mounted=false
cleanup_step() {
    local check=$1
    shift
    if ! "$@"; then
        echo "scope=linux-selinux-prototype classification=cleanup-incomplete check=$check" >&2
        return 1
    fi
}
cleanup() {
    local status=$? cleanup_failed=0 directory
    trap - EXIT
    # A busy mount or failed removal must not leave global enforcement changed.
    if ! cleanup_step enforcement setenforce "$previous"; then cleanup_failed=1; fi
    if [[ $alias_mounted == true ]]; then
        if cleanup_step unmount umount "$alias"; then
            alias_mounted=false
        else
            cleanup_failed=1
        fi
    fi
    for directory in "${owned_directories[@]}"; do
        # Retain a busy alias's source rather than delete through a live view.
        if [[ $directory == "$installation/custody" && $alias_mounted == true ]]; then continue; fi
        if ! cleanup_step fixture-removal rm -rf -- "$directory"; then cleanup_failed=1; fi
    done
    if [[ $alias_created == true && $alias_mounted == false ]]; then
        if ! cleanup_step alias-removal rmdir -- "$alias"; then cleanup_failed=1; fi
    fi
    if [[ $registration_mode_changed == true ]]; then
        if ! cleanup_step registration-mode chmod "$registration_mode" "$registration"; then cleanup_failed=1; fi
    fi
    if [[ $status == 0 && $cleanup_failed != 0 ]]; then status=1; fi
    exit "$status"
}
trap cleanup EXIT
setenforce 1
[[ $(getenforce) == Enforcing ]] || fail enforcing
if seinfo --permissive -x | grep -qw stln_probe_t; then fail domain-permissive; fi
if sesearch -A -s stln_probe_t -t stln_custody_t -c file -p write | grep -q '^allow '; then
    fail effective-write-allow
fi
# Existing prototype records are not promoted to authority by relabeling them.
# Fresh test inodes are created under enforcing policy before DAC exposure.
registration_mode_changed=true
chmod 0755 "$registration"
uid=$(id -u sdk)
gid=$(id -g sdk)
for directory in "$installation/custody" "$registration/custody"; do
    mkdir -m 0700 "$directory"
    owned_directories+=("$directory")
    truncate -s 4096 "$directory/file"
    truncate -s 4096 "$directory/victim"
    restorecon -RF "$directory"
    chown "$uid:$gid" "$directory" "$directory/file" "$directory/victim"
    chmod 0777 "$directory"
    chmod 0666 "$directory/file" "$directory/victim"
done
mkdir -m 0755 "$alias"
alias_created=true
mount --bind "$installation/custody" "$alias"
alias_mounted=true
mkdir -m 0700 "$control"
owned_directories+=("$control")
for selector in installation registration alias; do
    install -d -o sdk -g sdk -m 0777 "$control"
    truncate -s 4096 "$control/file"
    chown sdk:sdk "$control/file"
    chmod 0666 "$control/file"
    install -o sdk -g sdk -m 0666 /dev/null "$control/result"
    restorecon -RF "$control"
    case $selector in
        installation) object=$installation/custody ;;
        registration) object=$registration/custody ;;
        alias) object=$alias ;;
    esac
    # The descriptor predates the subject transition, not policy installation.
    if ! runuser -u sdk -- "$installation/custody-probe" "$selector" \
        4<>"$object/file" >"$control/result" 2>&1; then
        cat "$control/result"
        fail confined-probe
    fi
    cat "$control/result"
    grep -Fq 'denied_operations=12 retained_fd=4 read_control=passed write_control=passed mapping_control=passed entry_control=passed' \
        "$control/result" || fail confined-result
done
# This counterexample is part of the evidence. Stock account domains remain
# unconfined, so this module cannot satisfy full account publication custody.
runuser -u sdk -- "$installation/custody-probe" installation --unconfined-control
printf 'scope=linux-selinux-prototype confined_denials=36 ordinary_alias=proved full_account_custody=unproven boot_enforcement=unproven\n'
