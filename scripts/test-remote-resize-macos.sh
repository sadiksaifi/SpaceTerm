#!/bin/bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "the Remote Pane resize integration test requires macOS" >&2
  exit 1
fi

fixture="$(mktemp -d /private/tmp/sptrr.XXXXXX)"
sshd_pid=""

cleanup() {
  if [[ -n "$sshd_pid" ]]; then
    kill "$sshd_pid" 2>/dev/null || true
    wait "$sshd_pid" 2>/dev/null || true
  fi
  rm -rf -- "$fixture"
}
trap cleanup EXIT INT TERM

address="${SPACETERM_LOCAL_SSH_ADDRESS:-127.0.0.1}"
if [[ -z "${SPACETERM_LOCAL_SSH_ADDRESS:-}" ]] && pgrep -x OrbStack >/dev/null; then
  orbstack_bridge="$(ipconfig getifaddr bridge100 2>/dev/null || true)"
  if [[ -n "$orbstack_bridge" ]]; then
    address="$orbstack_bridge"
  fi
fi
port="$(python3 - "$address" <<'PY'
import socket
import sys

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
    listener.bind((sys.argv[1], 0))
    print(listener.getsockname()[1])
PY
)"
user="$(id -un)"

chmod 700 "$fixture"
ssh-keygen -q -t ed25519 -N "" -f "$fixture/host-key"
ssh-keygen -q -t ed25519 -N "" -f "$fixture/client-key"
chmod 600 "$fixture/host-key" "$fixture/client-key" "$fixture/client-key.pub"

host_key="$(awk '{ print $1 " " $2 }' "$fixture/host-key.pub")"
printf '[%s]:%s %s\n' "$address" "$port" "$host_key" >"$fixture/known-hosts"
cat >"$fixture/client-config" <<EOF
Host spaceterm-local
    HostName $address
    Port $port
    User $user
    IdentityFile $fixture/client-key
    IdentitiesOnly yes
    BatchMode yes
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    StrictHostKeyChecking yes
    UserKnownHostsFile $fixture/known-hosts
    LogLevel ERROR
EOF
chmod 600 "$fixture/client-config" "$fixture/known-hosts"

/usr/sbin/sshd -D -e -f /dev/null \
  -p "$port" \
  -h "$fixture/host-key" \
  -o "ListenAddress=$address" \
  -o "PidFile=$fixture/sshd.pid" \
  -o "AuthorizedKeysFile=$fixture/client-key.pub" \
  -o "AllowUsers=$user" \
  -o PubkeyAuthentication=yes \
  -o PasswordAuthentication=no \
  -o KbdInteractiveAuthentication=no \
  -o UsePAM=no \
  -o StrictModes=no \
  -o PermitRootLogin=no \
  -o DisableForwarding=yes \
  -o LogLevel=ERROR \
  >"$fixture/sshd.log" 2>&1 &
sshd_pid="$!"

ready=false
for _ in {1..100}; do
  if /usr/bin/ssh -F "$fixture/client-config" spaceterm-local true 2>/dev/null; then
    ready=true
    break
  fi
  if ! kill -0 "$sshd_pid" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
if [[ "$ready" != true ]]; then
  echo "the temporary local sshd did not become ready" >&2
  exit 1
fi

SPACETERM_LOCAL_SSH_CONFIG="$fixture/client-config" \
  ./scripts/cargo-artifacts.sh run -- \
  cargo test --package spaceterm --features macos-native-tests --locked \
  platform::macos_ssh_process::tests::remote_pane_resize_reaches_a_local_openssh_server -- \
  --ignored --exact
