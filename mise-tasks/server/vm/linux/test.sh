#!/usr/bin/env bash
# Portable bounded-registration tests. Linux additionally builds the ELF prototype.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../../.." && pwd)
build=$(mktemp -d)
trap 'rm -rf -- "$build"' EXIT
flags=(-std=c11 -O2 -Wall -Wextra -Werror -pedantic)
if [[ ${1:-} == --sanitizers && $# == 1 ]]; then
    flags+=('-fsanitize=address,undefined' -fno-omit-frame-pointer)
elif [[ $# != 0 ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
source_dir=$root/tests/server_native/linux
cc "${flags[@]}" "$source_dir/registration.c" "$source_dir/registration_tests.c" -o "$build/registration-tests"
"$build/registration-tests"
if [[ $(uname -s) == Linux ]]; then
    for program in launcher control; do
        cc "${flags[@]}" "$source_dir/registration.c" "$source_dir/$program.c" -o "$build/$program"
    done
    for program in server ptrace_probe mount_evidence; do
        cc "${flags[@]}" "$source_dir/$program.c" -o "$build/$program"
    done
    cc "${flags[@]}" "$source_dir/custody_probe.c" -lselinux -o "$build/custody-probe"
    echo 'scope=linux-launcher-prototype case=build result=passed programs=6'
fi
