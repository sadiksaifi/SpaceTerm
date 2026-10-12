#!/usr/bin/env -S python -I
# MISE description="Request bounded disposable VM preparation; execution unavailable"
# MISE confirm="Prepare only the disposable VM directory without changing penguin's OS?"
# USAGE arg "<directory>" help="New empty VM directory outside every checkout"
# USAGE arg "<image>" help="Ubuntu 22.04 image in its own download directory"
# USAGE flag "--checksums <checksums>" help="Canonical signed checksum data" required=true
# USAGE flag "--signature <signature>" help="Detached checksum signature" required=true
# USAGE flag "--sha256 <sha256>" help="Independently authenticated image digest" required=true
# USAGE flag "--signer-fingerprint <signer_fingerprint>" help="Independently trusted signer" required=true
# USAGE flag "--runner-image <runner_image>" help="Authenticated digest-pinned QEMU runner" required=true
# USAGE flag "--cpus <cpus>" help="Guest CPUs, at most 2" default="2"
# USAGE flag "--memory-mib <memory_mib>" help="Guest RAM in MiB, at most 4096" default="4096"
# USAGE flag "--disk-gib <disk_gib>" help="Guest OS disk in GiB, at most 24" default="24"
# USAGE flag "--encrypted-disk-gib <encrypted_disk_gib>" help="Encrypted-data disk in GiB, at most 8" default="8"
# USAGE flag "--boot-timeout-seconds <boot_timeout_seconds>" help="Finite boot deadline" default="180"
# USAGE flag "--ssh-port <ssh_port>" help="Unprivileged host port bound only to 127.0.0.1" default="25022"
"""Execution gate. Read no files, download nothing, and return gate-not-ready."""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("phase=prepare classification=isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    # -I ignores PYTHONPATH and the current directory. Add only the trusted task library.
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.server_vm import (
        GuestImage,
        Phase,
        PrepareRequest,
        Resources,
        SshEndpoint,
        VmArgumentParser,
        run_task,
    )

    parser = VmArgumentParser(Phase.PREPARE, description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("image", type=Path)
    parser.add_argument("--checksums", type=Path, required=True)
    parser.add_argument("--signature", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--signer-fingerprint", required=True)
    parser.add_argument("--runner-image", required=True)
    parser.add_argument("--cpus", type=int, default=2)
    parser.add_argument("--memory-mib", type=int, default=4096)
    parser.add_argument("--disk-gib", type=int, default=24)
    parser.add_argument("--encrypted-disk-gib", type=int, default=8)
    parser.add_argument("--boot-timeout-seconds", type=int, default=180)
    parser.add_argument("--ssh-port", type=int, default=25022)
    args = parser.parse_args()
    request = PrepareRequest(
        directory=args.directory,
        image=GuestImage(
            path=args.image,
            checksums=args.checksums,
            signature=args.signature,
            sha256=args.sha256,
            signer_fingerprint=args.signer_fingerprint,
        ),
        runner_image=args.runner_image,
        resources=Resources(
            cpus=args.cpus,
            memory_mib=args.memory_mib,
            disk_gib=args.disk_gib,
            encrypted_disk_gib=args.encrypted_disk_gib,
            boot_timeout_seconds=args.boot_timeout_seconds,
        ),
        ssh=SshEndpoint(port=args.ssh_port),
    )
    main(lambda: run_task(Phase.PREPARE, request=request))
