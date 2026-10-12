#!/usr/bin/env bash
# Install build tools only inside the explicitly selected disposable Jammy guest.
set -euo pipefail

if [[ ${1:-} != --disposable-jammy || $# != 1 ]]; then
    echo 'classification=invalid-request' >&2
    exit 2
fi
# shellcheck source=/dev/null
source /etc/os-release
if [[ $ID != ubuntu || $VERSION_ID != 22.04 || $(id -un) != sdk || $(uname -m) != x86_64 ]]; then
    echo 'classification=unsupported-guest' >&2
    exit 2
fi
sudo -n env DEBIAN_FRONTEND=noninteractive apt-get update
sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential pkg-config ca-certificates curl git rsync strace libcap2-bin \
    shellcheck util-linux libselinux1-dev

if ! [[ -x $HOME/.cargo/bin/rustup ]]; then
    download=$(mktemp -d)
    trap 'rm -rf -- "$download"' EXIT
    curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
        https://sh.rustup.rs --output "$download/rustup-init.sh"
    # The installer lives in a new download directory, not the script's cwd.
    sh "$download/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain none
fi
"$HOME/.cargo/bin/rustup" toolchain install 1.98.1 --profile minimal --component clippy,rustfmt
printf 'kernel=%s\n' "$(uname -r)"
getconf GNU_LIBC_VERSION
dpkg-query -W -f='${Package}=${Version}\n' libc6 systemd
"$HOME/.cargo/bin/rustc" +1.98.1 --version
"$HOME/.cargo/bin/cargo" +1.98.1 --version
