"""Bounds and image admission for the coordinator's disposable Jammy VM.

VM data and downloaded inputs are outside checkouts. There is no native evidence
receipt, credential test, encrypted data disk, or service-manager mutation here.
"""

import hashlib
import os
import re
import stat
from dataclasses import dataclass, field
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks import server_vm_linux as native

GIB = 1024**3
IMAGE_NAME = "jammy-server-cloudimg-amd64.img"


def require_mutation(allowed):
    if allowed is not True:
        raise TaskError("explicit-vm-mutation-required")
    if os.getuid() == 0:
        raise TaskError("root-vm-runner-refused")


@dataclass(frozen=True)
class Resources:
    cpus: int = 2
    memory_mib: int = 4096
    disk_gib: int = 24
    boot_timeout_seconds: int = 180

    def validate(self):
        if any(
            type(value) is not int or not minimum <= value <= maximum
            for value, minimum, maximum in (
                (self.cpus, 1, 2),
                (self.memory_mib, 512, 4096),
                (self.disk_gib, 8, 24),
                (self.boot_timeout_seconds, 1, 180),
            )
        ):
            raise TaskError("invalid-vm-resources")


@dataclass(frozen=True, repr=False)
class GuestImage:
    path: Path
    checksums: Path
    signature: Path
    keyring: Path
    sha256: str
    size_bytes: int
    signer_fingerprint: str
    keyring_sha256: str


@dataclass(frozen=True, repr=False)
class PrepareRequest:
    directory: Path
    image: GuestImage
    runner_image: str
    resources: Resources = field(default_factory=Resources)
    ssh_port: int = 25022

    def validate(self):
        self.resources.validate()
        if (
            type(self.ssh_port) is not int
            or not 1024 <= self.ssh_port <= 65535
            or not re.fullmatch(r"[a-z0-9./:_-]+@sha256:[0-9a-f]{64}", self.runner_image)
        ):
            raise TaskError("invalid-vm-target")
        private_directory(self.directory)
        if any(self.directory.iterdir()):
            raise TaskError("vm-directory-not-empty")


def private_directory(path):
    if not path.is_absolute() or path != path.resolve() or any(c in str(path) for c in ",:\n\r"):
        raise TaskError("unsafe-vm-directory")
    if path == ROOT or ROOT in path.parents or any((p / ".git").exists() for p in path.parents):
        raise TaskError("vm-directory-in-checkout")
    info = path.lstat()
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) != 0o700
    ):
        raise TaskError("unsafe-vm-directory")
    return info.st_dev, info.st_ino


def copy_verified_image(image, destination):
    """Copy and hash the authenticated image through one retained descriptor.

    Caller pins are independent authority. Downloaded checksums alone cannot
    choose a digest or signer. The original path is never passed to qemu-img.
    """
    if (
        image.path.name != IMAGE_NAME
        or not re.fullmatch(r"[0-9a-f]{64}", image.sha256)
        or type(image.size_bytes) is not int
        or not 0 < image.size_bytes <= 8 * GIB
    ):
        raise TaskError("invalid-image-pin")
    if len({p.parent for p in (image.path, image.checksums, image.signature, image.keyring)}) != 4:
        raise TaskError("image-inputs-require-separate-directories")
    for path in (image.path, image.checksums, image.signature, image.keyring):
        private_directory(path.parent)
    checksums = native.verify_checksums(
        image.checksums,
        image.signature,
        image.keyring,
        image.signer_fingerprint,
        image.keyring_sha256,
    )
    if checksums.get(IMAGE_NAME) != image.sha256:
        raise TaskError("image-pin-mismatch")
    fd = os.open(image.path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_nlink != 1
            or before.st_size != image.size_bytes
        ):
            raise TaskError("unsafe-image-input")
        digest = hashlib.sha256()
        total = 0
        with destination.open("xb") as target:
            destination.chmod(0o600)
            while data := os.read(fd, 1024 * 1024):
                total += len(data)
                if total > image.size_bytes:
                    raise TaskError("image-input-changed")
                digest.update(data)
                target.write(data)
            target.flush()
            os.fsync(target.fileno())
        if (
            total != image.size_bytes
            or digest.hexdigest() != image.sha256
            or os.fstat(fd) != before
        ):
            raise TaskError("image-pin-mismatch")
    except BaseException:
        destination.unlink(missing_ok=True)
        raise
    finally:
        os.close(fd)


def run_task(phase, arguments):
    """Explicit owner entry points. No normal validation task calls them."""
    import argparse

    from spaceterm_tasks.server_vm_executor import VmExecutor

    parser = argparse.ArgumentParser(description="Operate only the owned disposable Jammy VM")
    parser.add_argument("directory", type=lambda value: Path(value).absolute())
    parser.add_argument("--allow-mutation", action="store_true")
    if phase == "prepare":
        parser.add_argument("image", type=Path)
        for option in ("checksums", "signature", "keyring"):
            parser.add_argument("--" + option, type=Path, required=True)
        for option in ("sha256", "signer-fingerprint", "keyring-sha256", "runner-image"):
            parser.add_argument("--" + option, required=True)
        parser.add_argument("--size-bytes", type=int, required=True)
        parser.add_argument("--ssh-port", type=int, default=25022)
    args = parser.parse_args(arguments)
    require_mutation(args.allow_mutation)
    executor = VmExecutor(args.directory)
    if phase == "prepare":
        image = GuestImage(
            args.image,
            args.checksums,
            args.signature,
            args.keyring,
            args.sha256,
            args.size_bytes,
            args.signer_fingerprint,
            args.keyring_sha256,
        )
        executor.prepare(
            PrepareRequest(args.directory, image, args.runner_image, ssh_port=args.ssh_port),
            allow_mutation=True,
        )
    elif phase in ("start", "stop", "remove"):
        getattr(executor, phase)(allow_mutation=True)
    else:
        raise TaskError("invalid-vm-operation")
    print(f"phase={phase} classification=completed")
    return 0
