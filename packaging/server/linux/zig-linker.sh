#!/bin/sh
# Link against the supported Ubuntu 22.04 ABI, not the build host's newer libc.
set -eu
exec zig cc -target x86_64-linux-gnu.2.35 "$@"
