#!/usr/bin/env -S python -I
# MISE description="Prepare the owned disposable Jammy VM"
# MISE confirm="Prepare only the owned disposable VM without changing the host OS?"
# USAGE arg "<directory>" help="Existing empty 0700 directory outside checkouts"
# USAGE flag "--allow-mutation" help="Explicitly permit this VM operation"
# USAGE arg "<image>" help="Ubuntu 22.04 image in its own download directory"
# USAGE flag "--checksums <checksums>" help="Signed checksums in a separate directory" required=true
# USAGE flag "--signature <signature>" help="Detached checksum signature" required=true
# USAGE flag "--keyring <keyring>" help="Independently trusted Ubuntu keyring" required=true
# USAGE flag "--sha256 <sha256>" help="Independently authenticated image digest" required=true
# USAGE flag "--size-bytes <size_bytes>" help="Independently authenticated image size" required=true
# USAGE flag "--signer-fingerprint <signer_fingerprint>" help="Trusted primary signer" required=true
# USAGE flag "--keyring-sha256 <keyring_sha256>" help="Independent keyring digest" required=true
# USAGE flag "--runner-image <runner_image>" help="Authenticated digest-pinned QEMU runner" required=true
# USAGE flag "--ssh-port <ssh_port>" help="Loopback-only SSH port" default="25022"
"""Operate only an explicitly selected owned disposable VM."""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.server_vm import run_task

    main(lambda: run_task("prepare", sys.argv[1:]))
