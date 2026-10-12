"""Non-executing admission and lifecycle plans for one disposable QEMU VM.

The planner reads real filesystem identities, signed checksums, and the image. It
never downloads, generates credentials, writes a receipt, or operates a container.
Only detached-signature verification and exact-ID container inspection cross
injected native boundaries. Pins and the retained receipt come from independent
local authority, never from a newly read manifest or downloaded checksum file.

Plans are not execution authority or Ubuntu/systemd/boot evidence. Execution must
reopen retained identities through directory handles, revalidate independently
pinned configuration, and verify prepared virtual disk sizes before use.
In particular, remove_directory is not permission for path-only rmtree. Shared
images, foreign containers, and volumes are never cleanup targets.
"""

import argparse
import errno
import hashlib
import json
import os
import re
import stat
import sys
from collections.abc import Iterable, Iterator
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass, field, fields, is_dataclass, replace
from enum import StrEnum
from pathlib import Path
from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from spaceterm_tasks.server_vm_executor import ExecutionReceipt, VmExecutor

from spaceterm_tasks import ROOT

MANIFEST_NAME = ".spaceterm-vm.json"
MANIFEST_SCHEMA = 1
OWNER_LABEL = "io.github.sadiksaifi.spaceterm.issue520.vm-owner"
_MAX_DOCUMENT_BYTES = 64 * 1024
_MAX_SIGNATURE_BYTES = 64 * 1024
_MAX_IMAGE_BYTES = 8 * 1024**3
_MAX_SEED_BYTES = 4 * 1024**2
_MAX_DISK_OVERHEAD_BYTES = 1024**3
_MAX_CHECKSUM_ENTRIES = 256
_HASH_CHUNK_BYTES = 1024 * 1024
_HEX_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
_FINGERPRINT = re.compile(r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})\Z")
_PATH_COMPONENT = re.compile(r"[A-Za-z0-9_. +\-]+\Z")
_REPOSITORY_COMPONENT = re.compile(r"[a-z0-9]+(?:[._\-][a-z0-9]+)*\Z")
_CHECKSUM_LINE = re.compile(r"([0-9a-f]{64}) [ *]([A-Za-z0-9_][A-Za-z0-9_.+\-]{0,254})\Z")


class Phase(StrEnum):
    PREPARE = "prepare"
    START = "start"
    STOP = "stop"
    REMOVE = "remove"


class Classification(StrEnum):
    GATE_NOT_READY = "gate-not-ready"
    PLANNED = "planned"
    INVALID_REQUEST = "invalid-request"
    INVALID_RESOURCES = "invalid-resources"
    UNSAFE_PATH = "unsafe-path"
    INVALID_ENDPOINT = "invalid-endpoint"
    UNVERIFIED_IMAGE = "unverified-image"
    OWNERSHIP_MISMATCH = "ownership-mismatch"
    INVALID_STATE = "invalid-state"
    NATIVE_UNAVAILABLE = "native-unavailable"
    PREPARED = "prepared"
    STARTED = "started"
    STOPPED = "stopped"
    REMOVED = "removed"
    CLEANUP_INCOMPLETE = "cleanup-incomplete"
    CREATION_UNCERTAIN = "creation-uncertain"


@dataclass(frozen=True, slots=True)
class Resources:
    """Guest bounds, not host capacity or proof of prepared virtual disk sizes.

    The runner has separate fixed limits: 2 CPUs, 6 GiB memory and memory-swap,
    256 PIDs, and bounded tmpfs/logs. Host storage admission remains an execution
    prerequisite. Values must be integers, not bools.
    """

    cpus: int = field(default=2, repr=False)
    memory_mib: int = field(default=4096, repr=False)
    disk_gib: int = field(default=24, repr=False)
    encrypted_disk_gib: int = field(default=8, repr=False)
    boot_timeout_seconds: int = field(default=180, repr=False)


@dataclass(frozen=True, slots=True)
class RunnerIdentity:
    """Nonroot numeric uid:gid and only /dev/kvm's numeric supplemental group.

    These injected native facts grant no host group or device permission changes.
    """

    uid: int = field(repr=False)
    gid: int = field(repr=False)
    kvm_gid: int = field(repr=False)


@dataclass(frozen=True, slots=True)
class SshEndpoint:
    """Only 127.0.0.1 and unprivileged ports may be published on the host."""

    address: str = field(default="127.0.0.1", repr=False)
    port: int = field(default=25022, repr=False)


@dataclass(frozen=True, slots=True)
class GuestImage:
    """Downloaded data plus separately authenticated image and signer pins.

    Each object belongs in its own private download directory outside checkouts.
    No downloaded code is executed. Read these objects with Python -I and keep
    trusted scripts elsewhere. The image is hashed in bounded streaming reads.
    """

    path: Path = field(repr=False)
    checksums: Path = field(repr=False)
    signature: Path = field(repr=False)
    sha256: str | None = field(repr=False)
    signer_fingerprint: str = field(repr=False)
    # Independent authenticated pin, not a size learned from the downloaded file.
    # None keeps the nonexecuting CLI constructible, but must fail admission.
    size_bytes: int | None = field(default=None, repr=False)


@dataclass(frozen=True, slots=True)
class PrepareRequest:
    """A fresh, empty, private VM directory and a bounded guest configuration.

    runner_image must match the planner's independently authenticated immutable
    runner_image_pin. Supplying a digest in this request alone is not proof.
    """

    directory: Path = field(repr=False)
    image: GuestImage = field(repr=False)
    runner_image: str = field(repr=False)
    resources: Resources = field(default_factory=Resources, repr=False)
    ssh: SshEndpoint = field(default_factory=SshEndpoint, repr=False)


@dataclass(frozen=True, slots=True)
class OwnedPath:
    """An independently retained inode and root-to-parent ancestry.

    capture uses no-follow directory-relative opens, then closes its handles.
    It also hashes regular files up to the document bound. Credential files must
    retain that content pin; manifests instead match the independent receipt.
    Admission reopens and compares those identities while holding real handles.
    A caller must retain this value before replacement, not recreate it from an
    untrusted manifest when asking to stop or remove an existing VM.
    """

    path: Path = field(repr=False)
    device: int = field(repr=False)
    inode: int = field(repr=False)
    ancestors: tuple[tuple[int, int], ...] = field(default=(), repr=False)
    content_sha256: str | None = field(default=None, repr=False, compare=False)

    @classmethod
    def capture(cls, path: Path) -> "OwnedPath":
        """Capture a real identity; failures contain only a bounded classification."""
        try:
            with _open_path(path, Classification.UNSAFE_PATH) as opened:
                digest = None
                if (
                    stat.S_ISREG(opened.observation.st_mode)
                    and opened.observation.st_size <= _MAX_DOCUMENT_BYTES
                ):
                    data = (
                        _read_bounded(opened, _MAX_DOCUMENT_BYTES, Classification.UNSAFE_PATH)
                        if opened.observation.st_size
                        else b""
                    )
                    digest = hashlib.sha256(data).hexdigest()
                _unchanged(opened, Classification.UNSAFE_PATH)
                return cls(
                    path,
                    opened.owned.device,
                    opened.owned.inode,
                    opened.owned.ancestors,
                    digest,
                )
        except OSError:
            raise _Denied(Classification.NATIVE_UNAVAILABLE) from None


@dataclass(frozen=True, slots=True)
class Ownership:
    """Independent receipt binding token, checkout, paths, keys, and exact ID.

    The executor creates the 256-bit token and requests disposable guest
    credentials. This planner neither creates them nor learns ownership from
    a container name, label search, or replacement manifest.
    """

    token: str = field(repr=False)
    checkout: OwnedPath = field(repr=False)
    directory: OwnedPath = field(repr=False)
    manifest: OwnedPath = field(repr=False)
    credential_directory: OwnedPath = field(repr=False)
    credential_files: tuple[OwnedPath, ...] = field(repr=False)
    container_id: str | None = field(default=None, repr=False)


@dataclass(frozen=True, slots=True)
class DiskEvidence:
    """Retained file and offline native facts, never an uncreated disk proposal."""

    file: OwnedPath = field(repr=False)
    format: str = field(repr=False)
    virtual_size_bytes: int = field(repr=False)
    has_backing_file: bool = field(repr=False)
    has_external_data_file: bool = field(repr=False)
    allocated_bytes: int = field(repr=False)


@dataclass(frozen=True, slots=True)
class SeedEvidence:
    """Exact retained NoCloud ISO bytes, bounded to 4 MiB and labelled CIDATA."""

    file: OwnedPath = field(repr=False)
    size_bytes: int = field(repr=False)
    sha256: str = field(repr=False)
    label: str = field(repr=False)


@dataclass(frozen=True, slots=True)
class VmArtifacts:
    """Independent evidence for both standalone disks and the read-only seed."""

    os_disk: DiskEvidence = field(repr=False)
    encrypted_disk: DiskEvidence = field(repr=False)
    seed: SeedEvidence = field(repr=False)


@dataclass(frozen=True, slots=True)
class PreparedVm:
    """Retained configuration and ownership, supplied independently of the manifest.

    Start requires existing private disks and exact bounded CIDATA bytes. Native
    offline facts and source pins come from the independent receipt, not this VM's
    manifest. Neither planning nor the data-disk filename proves LUKS encryption.
    """

    request: PrepareRequest = field(repr=False)
    ownership: Ownership = field(repr=False)
    artifacts: VmArtifacts | None = field(default=None, repr=False)


@dataclass(frozen=True, slots=True)
class ContainerConfiguration:
    """Native inspection facts required before restarting an existing container.

    No defaults stand in for native proof. The inspector reports actual effective
    limits, mounts, device mappings, namespaces, publication, and command. The
    planner compares all of them with its fixed runner policy.
    """

    runner_image: str = field(repr=False)
    runner: RunnerIdentity = field(repr=False)
    cpus: int = field(repr=False)
    memory_mib: int = field(repr=False)
    memory_swap_mib: int = field(repr=False)
    pids_limit: int = field(repr=False)
    read_only: bool = field(repr=False)
    privileged: bool = field(repr=False)
    cap_drop: tuple[str, ...] = field(repr=False)
    cap_add: tuple[str, ...] = field(repr=False)
    security_options: tuple[str, ...] = field(repr=False)
    supplemental_groups: tuple[int, ...] = field(repr=False)
    tmpfs: tuple[str, ...] = field(repr=False)
    devices: tuple[str, ...] = field(repr=False)
    bind_mounts: tuple[tuple[Path, str], ...] = field(repr=False)
    volume_mounts: tuple[str, ...] = field(repr=False)
    # network, pid, ipc, uts, userns modes, in that order.
    namespaces: tuple[str, str, str, str, str] = field(repr=False)
    publications: tuple[str, ...] = field(repr=False)
    publish_all: bool = field(repr=False)
    log_driver: str = field(repr=False)
    log_options: tuple[str, ...] = field(repr=False)
    entrypoint: tuple[str, ...] = field(repr=False)
    arguments: tuple[str, ...] = field(repr=False)


@dataclass(frozen=True, slots=True)
class RunnerPolicyObservation:
    """Complete additional effective Docker facts, compared without normalization.

    Native adapters must report or reject these fields, not drop dangerous values
    while normalizing inspection. There are no safe defaults for native proof.
    image_id is the actual local image object, not Docker's requested Config.Image.
    """

    image_id: str = field(repr=False)
    environment: tuple[str, ...] = field(repr=False)
    healthcheck: tuple[str, ...] = field(repr=False)
    declared_volumes: tuple[str, ...] = field(repr=False)
    declared_ports: tuple[str, ...] = field(repr=False)
    working_directory: str = field(repr=False)
    restart_policy: str = field(repr=False)
    restart_retries: int = field(repr=False)
    auto_remove: bool = field(repr=False)
    runtime: str = field(repr=False)
    device_requests: tuple[str, ...] = field(repr=False)
    device_cgroup_rules: tuple[str, ...] = field(repr=False)
    volumes_from: tuple[str, ...] = field(repr=False)
    bind_propagation: str = field(repr=False)
    recursive_submounts: bool = field(repr=False)
    cgroup_namespace: str = field(repr=False)
    seccomp_profile: str = field(repr=False)
    apparmor_profile: str = field(repr=False)
    resource_overrides: tuple[str, ...] = field(repr=False)
    oom_kill_disable: bool = field(repr=False)
    oom_score_adj: int = field(repr=False)
    sysctls: tuple[str, ...] = field(repr=False)
    cgroup_parent: str = field(repr=False)
    storage_options: tuple[str, ...] = field(repr=False)
    stop_signal: str = field(repr=False)
    stop_timeout_seconds: int = field(repr=False)


@dataclass(frozen=True, slots=True)
class ContainerObservation:
    """Exact native ID, label and VM mount, never raw Docker output or errors."""

    container_id: str = field(repr=False)
    owner_token: str | None = field(repr=False)
    mounted_directory: Path = field(repr=False)
    running: bool = field(repr=False)
    configuration: ContainerConfiguration | None = field(default=None, repr=False)
    policy: RunnerPolicyObservation | None = field(default=None, repr=False)


class SignatureVerifier(Protocol):
    def verify(self, checksums: bytes, signature: bytes) -> str | None:
        """Authenticate these exact bytes with a trusted keyring and return the signer.

        Both bounded byte strings come from the planner's held file descriptors.
        The adapter must not reopen their source paths, import keys, repair trust,
        or download data. The planner retains file and directory handles and
        checks their stability across this call. No arbitrary command is exposed.
        """
        ...


class ContainerInspector(Protocol):
    def inspect(self, container_id: str) -> ContainerObservation | None:
        """Read this exact ID; never operate on it or substitute a name lookup."""
        ...


@dataclass(frozen=True, slots=True)
class ImageEvidence:
    """Retained source identities, protection metadata, and authenticated pins.

    Files and their private download directories have matching image/checksum/
    signature order. Stamps include ownership, mode and change time. Revalidation
    rejects observed metadata changes; stamp equality is not mutation-history or
    atomic-byte proof. The held image bytes must also match the independent pins.
    """

    files: tuple[OwnedPath, OwnedPath, OwnedPath] = field(repr=False)
    directories: tuple[OwnedPath, OwnedPath, OwnedPath] = field(repr=False)
    sha256: str = field(repr=False)
    signer_fingerprint: str = field(repr=False)
    runner_image: str = field(repr=False)
    stamps: tuple[tuple[int, ...], ...] = field(repr=False)
    directory_stamps: tuple[tuple[int, ...], ...] = field(repr=False)
    size_bytes: int | None = field(default=None, repr=False)
    private_ancestors: tuple[OwnedPath, ...] = field(default=(), repr=False)
    ancestor_stamps: tuple[tuple[int, ...], ...] = field(default=(), repr=False)
    runner_image_id: str | None = field(default=None, repr=False)


@dataclass(frozen=True, slots=True)
class VmPlan:
    """Fixed argument vectors and exact retained identities, never shell strings.

    A plan does not authorize execution. Removal requires revalidation through
    retained directory identities, not rmtree on remove_directory. VM disks and
    credentials are still execution-layer prerequisites, not created by planning.
    """

    phase: Phase
    commands: tuple[tuple[str, ...], ...] = field(default=(), repr=False)
    ssh: SshEndpoint | None = field(default=None, repr=False)
    ownership: Ownership | None = field(default=None, repr=False)
    remove_directory: Path | None = field(default=None, repr=False)
    request: PrepareRequest | None = field(default=None, repr=False)
    image_evidence: ImageEvidence | None = field(default=None, repr=False)
    prepared_directory: OwnedPath | None = field(default=None, repr=False)
    checkout: OwnedPath | None = field(default=None, repr=False)
    runner: RunnerIdentity | None = field(default=None, repr=False)


@dataclass(frozen=True, slots=True)
class Decision:
    phase: Phase
    classification: Classification
    plan: VmPlan | None = field(default=None, repr=False)


class _Denied(Exception):
    """Content-free admission failure; never retains a path or native exception."""

    def __init__(self, classification: Classification):
        self.classification = classification
        super().__init__(classification.value)


@dataclass(frozen=True, slots=True)
class _OpenedPath:
    fd: int = field(repr=False)
    owned: OwnedPath = field(repr=False)
    observation: os.stat_result = field(repr=False)


def _safe_path(path: Path, failure: Classification) -> None:
    if not isinstance(path, Path) or not path.is_absolute() or len(str(path)) > 4096:
        raise _Denied(failure)
    if len(path.parts) < 2:
        raise _Denied(failure)
    for component in path.parts[1:]:
        if (
            component in (".", "..")
            or component.startswith(("-", " "))
            or not _PATH_COMPONENT.fullmatch(component)
            or len(component) > 255
        ):
            raise _Denied(failure)


def _close_descriptors(
    descriptors: Iterable[int],
    exiting_exception: BaseException | None,
) -> None:
    """Attempt each owned generation once, preserving the first interruption.

    A close error does not prove that the descriptor remains open. Retrying its
    number could close another owner's descriptor. The owning context supplies its
    actual exiting exception, or None on normal exit, not a caller's handled error.
    Release failures escape after every attempt unless a body interruption wins.
    """
    failure = (
        exiting_exception
        if exiting_exception is not None and not isinstance(exiting_exception, Exception)
        else None
    )
    for descriptor in descriptors:
        try:
            os.close(descriptor)
        except BaseException as error:
            if failure is None or (
                isinstance(failure, Exception) and not isinstance(error, Exception)
            ):
                failure = error
    if failure is not None and failure is not exiting_exception:
        raise failure


@contextmanager
def _closing_descriptor(descriptor: int) -> Iterator[int]:
    """Register a release that receives ExitStack's actual exiting exception."""
    exiting_exception: BaseException | None = None
    try:
        yield descriptor
    except BaseException as error:
        exiting_exception = error
        raise
    finally:
        _close_descriptors((descriptor,), exiting_exception)


@contextmanager
def _open_path(path: Path, failure: Classification) -> Iterator[_OpenedPath]:
    """Walk from / using held parent descriptors without following any symlink."""
    _safe_path(path, failure)
    descriptors: list[int] = []
    ancestors: list[tuple[int, int]] = []
    exiting_exception: BaseException | None = None
    try:
        directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        parent = os.open("/", directory_flags)
        descriptors.append(parent)
        for index, component in enumerate(path.parts[1:]):
            observation = os.fstat(parent)
            ancestors.append((observation.st_dev, observation.st_ino))
            final = index == len(path.parts) - 2
            flags = (
                os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK
                if final
                else directory_flags
            )
            parent = os.open(component, flags, dir_fd=parent)
            descriptors.append(parent)
        observation = os.fstat(parent)
        if not (stat.S_ISDIR(observation.st_mode) or stat.S_ISREG(observation.st_mode)):
            raise _Denied(failure)
        owned = OwnedPath(path, observation.st_dev, observation.st_ino, tuple(ancestors))
        yield _OpenedPath(parent, owned, observation)
    except OSError as error:
        if error.errno in (errno.ENOENT, errno.ENOTDIR, errno.ELOOP):
            exiting_exception = _Denied(failure)
            raise exiting_exception from None
        exiting_exception = error
        raise
    except BaseException as error:
        exiting_exception = error
        raise
    finally:
        _close_descriptors(reversed(descriptors), exiting_exception)


def _private(opened: _OpenedPath, *, directory: bool, failure: Classification) -> None:
    observation = os.fstat(opened.fd)
    kind = stat.S_ISDIR if directory else stat.S_ISREG
    if (
        not kind(observation.st_mode)
        or observation.st_uid != os.geteuid()
        or stat.S_IMODE(observation.st_mode) != (0o700 if directory else 0o600)
        or (not directory and observation.st_nlink != 1)
    ):
        raise _Denied(failure)


def _same_typed_value(actual: object, expected: object) -> bool:
    """Equality without bool/int coercion, extra fields, or untyped native facts."""
    if type(actual) is not type(expected):
        return False
    if isinstance(expected, dict) and isinstance(actual, dict):
        return actual.keys() == expected.keys() and all(
            _same_typed_value(actual[key], value) for key, value in expected.items()
        )
    if isinstance(expected, (tuple, list)) and isinstance(actual, (tuple, list)):
        return len(actual) == len(expected) and all(
            _same_typed_value(left, right) for left, right in zip(actual, expected, strict=True)
        )
    if is_dataclass(expected) and not isinstance(expected, type):
        return all(
            _same_typed_value(getattr(actual, item.name), getattr(expected, item.name))
            for item in fields(expected)
        )
    return actual == expected


def _read_bounded(opened: _OpenedPath, maximum: int, failure: Classification) -> bytes:
    if (
        not stat.S_ISREG(opened.observation.st_mode)
        or not 0 < opened.observation.st_size <= maximum
    ):
        raise _Denied(failure)
    os.lseek(opened.fd, 0, os.SEEK_SET)
    data = bytearray()
    while len(data) <= maximum:
        chunk = os.read(opened.fd, min(_HASH_CHUNK_BYTES, maximum + 1 - len(data)))
        if not chunk:
            break
        data.extend(chunk)
    if len(data) != opened.observation.st_size or len(data) > maximum:
        raise _Denied(failure)
    return bytes(data)


def _hash_file(opened: _OpenedPath, maximum: int, failure: Classification) -> tuple[int, str]:
    if (
        not stat.S_ISREG(opened.observation.st_mode)
        or not 0 < opened.observation.st_size <= maximum
    ):
        raise _Denied(failure)
    os.lseek(opened.fd, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    total = 0
    while chunk := os.read(opened.fd, min(_HASH_CHUNK_BYTES, maximum + 1 - total)):
        total += len(chunk)
        if total > maximum:
            raise _Denied(failure)
        digest.update(chunk)
    if total != opened.observation.st_size:
        raise _Denied(failure)
    return total, digest.hexdigest()


def _retained_file(
    opened: _OpenedPath, failure: Classification, *, maximum: int = _MAX_DOCUMENT_BYTES
) -> OwnedPath:
    """Pin every byte through bounded streaming reads, including prepared disks."""
    _private(opened, directory=False, failure=failure)
    _, digest = _hash_file(opened, maximum, failure)
    _unchanged(opened, failure)
    return replace(opened.owned, content_sha256=digest)


def _stamp(observation: os.stat_result) -> tuple[int, ...]:
    return (
        observation.st_dev,
        observation.st_ino,
        observation.st_mode,
        observation.st_uid,
        observation.st_gid,
        observation.st_nlink,
        observation.st_size,
        observation.st_mtime_ns,
        observation.st_ctime_ns,
    )


def _unchanged(opened: _OpenedPath, failure: Classification) -> None:
    if _stamp(os.fstat(opened.fd)) != _stamp(opened.observation):
        raise _Denied(failure)
    with _open_path(opened.owned.path, failure) as current:
        if current.owned != opened.owned or _stamp(current.observation) != _stamp(
            opened.observation
        ):
            raise _Denied(failure)


def _directory_protection(observation: os.stat_result) -> tuple[int, ...]:
    """Directory identity and protection, excluding metadata changed by entries."""
    return (
        observation.st_dev,
        observation.st_ino,
        observation.st_mode,
        observation.st_uid,
        observation.st_gid,
    )


def _unchanged_directory(opened: _OpenedPath, failure: Classification) -> None:
    """Allow entry changes, never a new root, ancestry, owner, group or mode."""
    expected = _directory_protection(opened.observation)
    _private(opened, directory=True, failure=failure)
    if _directory_protection(os.fstat(opened.fd)) != expected:
        raise _Denied(failure)
    with _open_path(opened.owned.path, failure) as current:
        _private(current, directory=True, failure=failure)
        if current.owned != opened.owned or _directory_protection(current.observation) != expected:
            raise _Denied(failure)
        if _directory_protection(os.fstat(current.fd)) != expected:
            raise _Denied(failure)
    if _directory_protection(os.fstat(opened.fd)) != expected:
        raise _Denied(failure)


def _local_image_id(value: str | None) -> bool:
    return (
        isinstance(value, str)
        and value.startswith("sha256:")
        and _HEX_DIGEST.fullmatch(value[7:]) is not None
    )


def _immutable_runner_image(value: str | None) -> bool:
    if _local_image_id(value):
        return True
    if not isinstance(value, str) or len(value) > 256 or value.count("@sha256:") != 1:
        return False
    repository, digest = value.split("@sha256:")
    if not _HEX_DIGEST.fullmatch(digest):
        return False
    components = repository.split("/")
    if ":" in components[0] and len(components) > 1:
        host, separator, port = components[0].partition(":")
        if (
            not separator
            or not port.isascii()
            or not port.isdecimal()
            or not 1 <= int(port) <= 65535
        ):
            return False
        components[0] = host
    return all(_REPOSITORY_COMPONENT.fullmatch(component) for component in components)


def _validate_resources(resources: Resources, runner: RunnerIdentity | None) -> None:
    if not isinstance(resources, Resources):
        raise _Denied(Classification.INVALID_RESOURCES)
    bounds = (
        (resources.cpus, 1, 2),
        (resources.memory_mib, 1024, 4096),
        (resources.disk_gib, 8, 24),
        (resources.encrypted_disk_gib, 1, 8),
        (resources.boot_timeout_seconds, 15, 300),
    )
    if any(
        type(value) is not int or not minimum <= value <= maximum
        for value, minimum, maximum in bounds
    ):
        raise _Denied(Classification.INVALID_RESOURCES)
    if runner is None:
        raise _Denied(Classification.NATIVE_UNAVAILABLE)
    if not isinstance(runner, RunnerIdentity) or any(
        type(value) is not int or not 1 <= value <= 2**31 - 1
        for value in (runner.uid, runner.gid, runner.kvm_gid)
    ):
        raise _Denied(Classification.INVALID_RESOURCES)


def _validate_endpoint(endpoint: SshEndpoint) -> None:
    if (
        not isinstance(endpoint, SshEndpoint)
        or endpoint.address != "127.0.0.1"
        or type(endpoint.port) is not int
        or not 1024 <= endpoint.port <= 65535
    ):
        raise _Denied(Classification.INVALID_ENDPOINT)


def _guest_pins(image: GuestImage) -> tuple[str, str]:
    if (
        not isinstance(image, GuestImage)
        or not isinstance(image.sha256, str)
        or not _HEX_DIGEST.fullmatch(image.sha256)
        or not isinstance(image.signer_fingerprint, str)
        or not _FINGERPRINT.fullmatch(image.signer_fingerprint)
        or type(image.size_bytes) is not int
        or not 0 < image.size_bytes <= _MAX_IMAGE_BYTES
    ):
        raise _Denied(Classification.UNVERIFIED_IMAGE)
    return image.sha256, image.signer_fingerprint


def _valid_owned_path(owned: OwnedPath) -> bool:
    return (
        isinstance(owned, OwnedPath)
        and isinstance(owned.path, Path)
        and type(owned.device) is int
        and owned.device >= 0
        and type(owned.inode) is int
        and owned.inode > 0
        and type(owned.ancestors) is tuple
        and len(owned.ancestors) == len(owned.path.parts) - 1
        and all(
            type(pair) is tuple
            and len(pair) == 2
            and type(pair[0]) is int
            and pair[0] >= 0
            and type(pair[1]) is int
            and pair[1] > 0
            for pair in owned.ancestors
        )
    )


def _admit_disk(disk: DiskEvidence, opened: _OpenedPath, expected_size: int) -> None:
    failure = Classification.OWNERSHIP_MISMATCH
    maximum = expected_size + _MAX_DISK_OVERHEAD_BYTES
    if (
        disk.format != "qcow2"
        or type(disk.virtual_size_bytes) is not int
        or disk.virtual_size_bytes != expected_size
        or disk.has_backing_file is not False
        or disk.has_external_data_file is not False
    ):
        raise _Denied(Classification.INVALID_STATE)
    if type(disk.allocated_bytes) is not int or not 0 < disk.allocated_bytes <= maximum:
        raise _Denied(Classification.INVALID_RESOURCES)
    _private(opened, directory=False, failure=failure)
    if opened.owned != disk.file:
        raise _Denied(failure)
    if (
        not 0 < opened.observation.st_size <= maximum
        or opened.observation.st_blocks * 512 != disk.allocated_bytes
    ):
        raise _Denied(Classification.INVALID_RESOURCES)
    if not isinstance(disk.file.content_sha256, str) or not _HEX_DIGEST.fullmatch(
        disk.file.content_sha256
    ):
        raise _Denied(failure)
    _, digest = _hash_file(opened, maximum, failure)
    if digest != disk.file.content_sha256:
        raise _Denied(failure)
    _unchanged(opened, failure)


def _admit_seed(seed: SeedEvidence, opened: _OpenedPath) -> None:
    failure = Classification.OWNERSHIP_MISMATCH
    if (
        seed.label != "CIDATA"
        or not isinstance(seed.sha256, str)
        or not _HEX_DIGEST.fullmatch(seed.sha256)
    ):
        raise _Denied(Classification.INVALID_STATE)
    if type(seed.size_bytes) is not int or not 0 < seed.size_bytes <= _MAX_SEED_BYTES:
        raise _Denied(Classification.INVALID_RESOURCES)
    _private(opened, directory=False, failure=failure)
    if opened.owned != seed.file:
        raise _Denied(failure)
    size, digest = _hash_file(opened, _MAX_SEED_BYTES, failure)
    if size != seed.size_bytes or digest != seed.sha256:
        raise _Denied(failure)
    _unchanged(opened, failure)


def _empty_directory(descriptor: int) -> bool:
    with os.scandir(descriptor) as entries:
        return next(entries, None) is None


def _directory_names(
    descriptor: int,
    required: frozenset[str],
    failure: Classification,
    *,
    optional: frozenset[str] = frozenset(),
) -> frozenset[str]:
    """Check a fixed namespace with at most allowed-count-plus-one entries.

    Callers supply only fixed artifact names. No directory-sized list or set is
    built. A foreign or duplicate entry closes admission immediately; missing
    required names also fail. The held-descriptor iterator closes on every exit.
    """
    allowed = required | optional
    seen: set[str] = set()
    with os.scandir(descriptor) as entries:
        for entry in entries:
            if len(seen) == len(allowed) or entry.name not in allowed or entry.name in seen:
                raise _Denied(failure)
            seen.add(entry.name)
    if not required <= seen:
        raise _Denied(failure)
    return frozenset(seen)


def _path_record(owned: OwnedPath) -> dict[str, object]:
    return {"path": str(owned.path), "device": owned.device, "inode": owned.inode}


def _artifact_record(artifacts: VmArtifacts | None) -> dict[str, object] | None:
    if artifacts is None:
        return None
    if (
        not isinstance(artifacts, VmArtifacts)
        or not isinstance(artifacts.os_disk, DiskEvidence)
        or not isinstance(artifacts.encrypted_disk, DiskEvidence)
        or not isinstance(artifacts.seed, SeedEvidence)
        or not all(
            _valid_owned_path(file)
            for file in (artifacts.os_disk.file, artifacts.encrypted_disk.file, artifacts.seed.file)
        )
    ):
        raise _Denied(Classification.INVALID_STATE)

    def disk_record(disk: DiskEvidence) -> dict[str, object]:
        return {
            "file": _path_record(disk.file),
            "format": disk.format,
            "virtual_size_bytes": disk.virtual_size_bytes,
            "has_backing_file": disk.has_backing_file,
            "has_external_data_file": disk.has_external_data_file,
            "allocated_bytes": disk.allocated_bytes,
        }

    return {
        "os_disk": disk_record(artifacts.os_disk),
        "encrypted_disk": disk_record(artifacts.encrypted_disk),
        "seed": {
            "file": _path_record(artifacts.seed.file),
            "size_bytes": artifacts.seed.size_bytes,
            "sha256": artifacts.seed.sha256,
            "label": artifacts.seed.label,
        },
    }


def _manifest(vm: PreparedVm) -> dict[str, object]:
    owner, request = vm.ownership, vm.request
    return {
        "schema": MANIFEST_SCHEMA,
        "token": owner.token,
        "checkout": _path_record(owner.checkout),
        "directory": _path_record(owner.directory),
        "manifest": _path_record(owner.manifest),
        "credential_directory": _path_record(owner.credential_directory),
        "credential_files": [_path_record(path) for path in owner.credential_files],
        "container_id": owner.container_id,
        "runner_image": request.runner_image,
        "guest_sha256": request.image.sha256,
        "guest_size_bytes": request.image.size_bytes,
        "guest_signer_fingerprint": request.image.signer_fingerprint,
        "artifacts": _artifact_record(vm.artifacts),
        "resources": {
            item.name: getattr(request.resources, item.name) for item in fields(Resources)
        },
        "ssh": {"address": request.ssh.address, "port": request.ssh.port},
    }


def _unique_json_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise _Denied(Classification.OWNERSHIP_MISMATCH)
        result[key] = value
    return result


def _invalid_json_constant(_value: str) -> object:
    raise _Denied(Classification.OWNERSHIP_MISMATCH)


def _guest_arguments(request: PrepareRequest) -> tuple[str, ...]:
    return (
        "-nodefaults",
        "-no-user-config",
        "-display",
        "none",
        "-monitor",
        "none",
        "-accel",
        "kvm",
        "-machine",
        "q35",
        "-cpu",
        "host",
        "-smp",
        str(request.resources.cpus),
        "-m",
        str(request.resources.memory_mib),
        "-drive",
        "file=/vm/os.qcow2,format=qcow2,if=virtio",
        "-drive",
        "file=/vm/encrypted.qcow2,format=qcow2,if=virtio",
        "-drive",
        "file=/vm/seed.iso,format=raw,if=ide,media=cdrom,readonly=on",
        "-netdev",
        "user,id=net0,hostfwd=tcp:0.0.0.0:2222-:22",
        "-device",
        "virtio-net-pci,netdev=net0",
        "-qmp",
        "unix:/vm/qmp.sock,server=on,wait=off",
        "-serial",
        "stdio",
    )


def _expected_configuration(
    request: PrepareRequest, runner: RunnerIdentity
) -> ContainerConfiguration:
    return ContainerConfiguration(
        runner_image=request.runner_image,
        runner=runner,
        cpus=2,
        memory_mib=6144,
        memory_swap_mib=6144,
        pids_limit=256,
        read_only=True,
        privileged=False,
        cap_drop=("ALL",),
        cap_add=(),
        security_options=("no-new-privileges:true",),
        supplemental_groups=(runner.kvm_gid,),
        tmpfs=("/tmp:rw,noexec,nosuid,size=64m",),
        devices=("/dev/kvm:/dev/kvm:rw",),
        bind_mounts=((request.directory, "/vm"),),
        volume_mounts=(),
        namespaces=("bridge", "", "private", "", ""),
        publications=(f"127.0.0.1:{request.ssh.port}:2222/tcp",),
        publish_all=False,
        log_driver="local",
        log_options=("max-size=10m", "max-file=2"),
        entrypoint=("/usr/bin/qemu-system-x86_64",),
        arguments=_guest_arguments(request),
    )


def _expected_policy(image_id: str) -> RunnerPolicyObservation:
    return RunnerPolicyObservation(
        image_id=image_id,
        environment=(),
        healthcheck=(),
        declared_volumes=(),
        declared_ports=(),
        working_directory="/vm",
        restart_policy="no",
        restart_retries=0,
        auto_remove=False,
        runtime="runc",
        device_requests=(),
        device_cgroup_rules=(),
        volumes_from=(),
        bind_propagation="rprivate",
        recursive_submounts=False,
        cgroup_namespace="private",
        seccomp_profile="default",
        apparmor_profile="docker-default",
        resource_overrides=(),
        oom_kill_disable=False,
        oom_score_adj=0,
        sysctls=(),
        cgroup_parent="",
        storage_options=(),
        stop_signal="SIGTERM",
        stop_timeout_seconds=30,
    )


def _start_command(vm: PreparedVm, runner: RunnerIdentity) -> tuple[str, ...]:
    request = vm.request
    return (
        "docker",
        "run",
        "--detach",
        "--cpus",
        "2",
        "--memory",
        "6g",
        "--memory-swap",
        "6g",
        "--pids-limit",
        "256",
        "--user",
        f"{runner.uid}:{runner.gid}",
        "--group-add",
        str(runner.kvm_gid),
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges:true",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=64m",
        "--device",
        "/dev/kvm:/dev/kvm:rw",
        "--mount",
        f"type=bind,src={request.directory},dst=/vm,bind-nonrecursive",
        "--network",
        "bridge",
        "--ipc",
        "private",
        "--cgroupns",
        "private",
        "--workdir",
        "/vm",
        "--runtime",
        "runc",
        "--stop-timeout",
        "30",
        "--publish",
        f"127.0.0.1:{request.ssh.port}:2222/tcp",
        "--label",
        f"{OWNER_LABEL}={vm.ownership.token}",
        "--log-driver",
        "local",
        "--log-opt",
        "max-size=10m",
        "--log-opt",
        "max-file=2",
        "--pull",
        "never",
        "--entrypoint",
        "/usr/bin/qemu-system-x86_64",
        request.runner_image,
        *_guest_arguments(request),
    )


class VmPlanner:
    """Explicit prepare/start/stop/remove plans with no implicit phase chaining.

    Signature and container adapters are read-only. The runner pin is supplied
    separately from requests. The checkout's identity is retained at construction
    and cannot be replaced by a matching foreign manifest. All native exceptions
    become bounded classifications, with no native messages or traceback data.
    """

    def __init__(
        self,
        checkout: Path = ROOT,
        signatures: SignatureVerifier | None = None,
        containers: ContainerInspector | None = None,
        runner: RunnerIdentity | None = None,
        runner_image_pin: str | None = None,
        runner_image_id_pin: str | None = None,
    ):
        self._checkout = checkout
        self._signatures = signatures
        self._containers = containers
        self._runner = runner
        self._runner_image_pin = runner_image_pin
        # RepoDigest and local image Id are different Docker identities. A repository
        # reference needs a separate authenticated object pin before actual inspection.
        self._runner_image_id_pin = (
            runner_image_pin
            if _local_image_id(runner_image_pin) and runner_image_id_pin is None
            else runner_image_id_pin
        )
        self._checkout_identity: OwnedPath | None = None
        self._checkout_failure: Classification | None = None
        try:
            with _open_path(checkout, Classification.UNSAFE_PATH) as opened:
                if not stat.S_ISDIR(opened.observation.st_mode):
                    raise _Denied(Classification.UNSAFE_PATH)
                self._checkout_identity = opened.owned
        except _Denied as denial:
            self._checkout_failure = denial.classification
        except OSError:
            self._checkout_failure = Classification.NATIVE_UNAVAILABLE

    def _configuration(self, request: PrepareRequest) -> None:
        if not isinstance(request, PrepareRequest):
            raise _Denied(Classification.INVALID_REQUEST)
        _validate_resources(request.resources, self._runner)
        _validate_endpoint(request.ssh)
        _guest_pins(request.image)
        if (
            not _immutable_runner_image(self._runner_image_pin)
            or not _immutable_runner_image(request.runner_image)
            or request.runner_image != self._runner_image_pin
            or (
                self._runner_image_id_pin is not None
                and not _local_image_id(self._runner_image_id_pin)
            )
            or (
                _local_image_id(self._runner_image_pin)
                and self._runner_image_id_pin != self._runner_image_pin
            )
        ):
            raise _Denied(Classification.UNVERIFIED_IMAGE)

    def _checkout_matches(self, failure: Classification) -> None:
        if self._checkout_failure is not None:
            raise _Denied(
                Classification.NATIVE_UNAVAILABLE
                if self._checkout_failure == Classification.NATIVE_UNAVAILABLE
                else failure
            )
        with _open_path(self._checkout, failure) as opened:
            if opened.owned != self._checkout_identity or not stat.S_ISDIR(
                opened.observation.st_mode
            ):
                raise _Denied(failure)

    def _outside_checkout(self, path: Path, failure: Classification) -> None:
        _safe_path(path, failure)
        if path.is_relative_to(self._checkout) or self._checkout.is_relative_to(path):
            raise _Denied(failure)
        # Reject other checkouts as well, without following their .git entries.
        for parent in (path, *path.parents[:-1]):
            with _open_path(parent, failure) as opened:
                if not stat.S_ISDIR(opened.observation.st_mode):
                    raise _Denied(failure)
                try:
                    os.stat(".git", dir_fd=opened.fd, follow_symlinks=False)
                except FileNotFoundError:
                    pass
                else:
                    raise _Denied(failure)

    def _image(self, image: GuestImage, directory: Path, handles: ExitStack) -> ImageEvidence:
        """Hold source files and private download directories until planning ends."""
        failure = Classification.UNVERIFIED_IMAGE
        digest_pin, signer_pin = _guest_pins(image)
        size_pin = image.size_bytes
        assert type(size_pin) is int  # _guest_pins admitted the independent exact size.
        if self._signatures is None:
            raise _Denied(Classification.NATIVE_UNAVAILABLE)
        paths = (image.path, image.checksums, image.signature)
        for path in paths:
            _safe_path(path, Classification.UNSAFE_PATH)
        parents = tuple(path.parent for path in paths)
        if len(set(parents)) != 3 or any(
            parent.is_relative_to(other) or other.is_relative_to(parent)
            for index, parent in enumerate(parents)
            for other in parents[index + 1 :]
        ):
            raise _Denied(Classification.UNSAFE_PATH)
        directories = []
        private_ancestors: dict[Path, _OpenedPath] = {}
        shared_ancestor = Path(os.path.commonpath((directory, self._checkout, *parents)))
        for parent in parents:
            self._outside_checkout(parent, Classification.UNSAFE_PATH)
            if parent.is_relative_to(directory) or directory.is_relative_to(parent):
                raise _Denied(Classification.UNSAFE_PATH)
            opened = handles.enter_context(_open_path(parent, Classification.UNSAFE_PATH))
            _private(opened, directory=True, failure=Classification.UNSAFE_PATH)
            directories.append(opened)
            # Retain download-only private ancestors, not shared host namespaces or
            # the common run root containing the checkout. Checkout exclusion still
            # walks the complete ancestry after every native callback.
            for ancestor in parent.parents:
                if ancestor in (shared_ancestor, Path("/")):
                    break
                candidate = handles.enter_context(_open_path(ancestor, Classification.UNSAFE_PATH))
                observation = candidate.observation
                if observation.st_uid == os.geteuid() and stat.S_IMODE(observation.st_mode) & 0o022:
                    raise _Denied(Classification.UNSAFE_PATH)
                if observation.st_uid != os.geteuid() or stat.S_IMODE(observation.st_mode) != 0o700:
                    break
                private_ancestors.setdefault(ancestor, candidate)
        actual, checksums, signature = opened_files = tuple(
            handles.enter_context(_open_path(path, Classification.UNSAFE_PATH)) for path in paths
        )
        for opened in opened_files:
            if (
                not stat.S_ISREG(opened.observation.st_mode)
                or opened.observation.st_nlink != 1
                or opened.observation.st_uid != os.geteuid()
                or stat.S_IMODE(opened.observation.st_mode) & 0o022
            ):
                raise _Denied(Classification.UNSAFE_PATH)
        if actual.observation.st_size != image.size_bytes:
            raise _Denied(failure)
        checksum_bytes = _read_bounded(checksums, _MAX_DOCUMENT_BYTES, failure)
        signature_bytes = _read_bounded(signature, _MAX_SIGNATURE_BYTES, failure)
        try:
            signer = self._signatures.verify(checksum_bytes, signature_bytes)
        except Exception:
            raise _Denied(Classification.NATIVE_UNAVAILABLE) from None
        # Native verification receives only held bytes. Its return does not prove
        # that their source files or private directories remained unchanged.
        for opened in opened_files:
            _unchanged(opened, failure)
        for opened in (*directories, *private_ancestors.values()):
            _private(opened, directory=True, failure=failure)
            _unchanged(opened, failure)
        if (
            not isinstance(signer, str)
            or not _FINGERPRINT.fullmatch(signer)
            or signer.upper() != signer_pin.upper()
        ):
            raise _Denied(failure)
        try:
            lines = checksum_bytes.decode("ascii").splitlines()
        except UnicodeError:
            raise _Denied(failure) from None
        if not 1 <= len(lines) <= _MAX_CHECKSUM_ENTRIES:
            raise _Denied(failure)
        entries: dict[str, str] = {}
        for line in lines:
            match = _CHECKSUM_LINE.fullmatch(line)
            if match is None or match[2] in entries:
                raise _Denied(failure)
            entries[match[2]] = match[1]
        if entries.get(image.path.name) != digest_pin:
            raise _Denied(failure)
        total, digest = _hash_file(actual, size_pin, failure)
        if total != size_pin or digest != digest_pin:
            raise _Denied(failure)
        for opened in opened_files:
            _unchanged(opened, failure)
        for opened in (*directories, *private_ancestors.values()):
            _private(opened, directory=True, failure=failure)
            _unchanged(opened, failure)
        return ImageEvidence(
            files=(
                actual.owned,
                replace(checksums.owned, content_sha256=hashlib.sha256(checksum_bytes).hexdigest()),
                replace(
                    signature.owned, content_sha256=hashlib.sha256(signature_bytes).hexdigest()
                ),
            ),
            directories=(directories[0].owned, directories[1].owned, directories[2].owned),
            sha256=digest_pin,
            signer_fingerprint=signer_pin,
            runner_image=self._runner_image_pin or "",
            stamps=tuple(_stamp(item.observation) for item in opened_files),
            directory_stamps=tuple(_stamp(item.observation) for item in directories),
            size_bytes=size_pin,
            private_ancestors=tuple(item.owned for item in private_ancestors.values()),
            ancestor_stamps=tuple(_stamp(item.observation) for item in private_ancestors.values()),
            runner_image_id=self._runner_image_id_pin,
        )

    def _hold_image(
        self, evidence: ImageEvidence, handles: ExitStack
    ) -> tuple[_OpenedPath, _OpenedPath, _OpenedPath]:
        failure = Classification.UNVERIFIED_IMAGE
        if (
            type(evidence.size_bytes) is not int
            or not 0 < evidence.size_bytes <= _MAX_IMAGE_BYTES
            or evidence.runner_image != self._runner_image_pin
            or evidence.runner_image_id != self._runner_image_id_pin
        ):
            raise _Denied(failure)
        for retained, stamp in zip(
            (*evidence.directories, *evidence.private_ancestors),
            (*evidence.directory_stamps, *evidence.ancestor_stamps),
            strict=True,
        ):
            opened = handles.enter_context(_open_path(retained.path, failure))
            self._outside_checkout(retained.path, failure)
            _private(opened, directory=True, failure=failure)
            if opened.owned != retained or _stamp(opened.observation) != stamp:
                raise _Denied(failure)
            _unchanged(opened, failure)
        opened_paths = tuple(
            handles.enter_context(_open_path(retained.path, failure)) for retained in evidence.files
        )
        for index, (retained, opened, stamp) in enumerate(
            zip(evidence.files, opened_paths, evidence.stamps, strict=True)
        ):
            observation = opened.observation
            if (
                opened.owned != retained
                or _stamp(observation) != stamp
                or not stat.S_ISREG(observation.st_mode)
                or observation.st_nlink != 1
                or observation.st_uid != os.geteuid()
                or stat.S_IMODE(observation.st_mode) & 0o022
            ):
                raise _Denied(failure)
            if index == 0:
                size, digest = _hash_file(opened, evidence.size_bytes, failure)
                if size != evidence.size_bytes or digest != evidence.sha256:
                    raise _Denied(failure)
            else:
                data = _read_bounded(opened, _MAX_DOCUMENT_BYTES, failure)
                if hashlib.sha256(data).hexdigest() != retained.content_sha256:
                    raise _Denied(failure)
            _unchanged(opened, failure)
        return opened_paths[0], opened_paths[1], opened_paths[2]

    def _image_matches(self, evidence: ImageEvidence) -> None:
        with ExitStack() as handles:
            self._hold_image(evidence, handles)

    def _ownership(self, vm: PreparedVm, *, runtime_started: bool = False) -> None:
        with ExitStack() as handles:
            self._hold_ownership(vm, handles, runtime_started=runtime_started)

    def _hold_ownership(
        self, vm: PreparedVm, handles: ExitStack, *, runtime_started: bool = False
    ) -> tuple[_OpenedPath, ...]:
        failure = Classification.OWNERSHIP_MISMATCH
        owner = vm.ownership
        if (
            not isinstance(owner, Ownership)
            or not all(
                _valid_owned_path(item)
                for item in (
                    owner.checkout,
                    owner.directory,
                    owner.manifest,
                    owner.credential_directory,
                )
            )
            or type(owner.credential_files) is not tuple
            or len(owner.credential_files) not in (2, 4)
            or not all(_valid_owned_path(item) for item in owner.credential_files)
        ):
            raise _Denied(failure)
        if (
            not isinstance(owner.token, str)
            or not _HEX_DIGEST.fullmatch(owner.token)
            or (
                owner.container_id is not None
                and (
                    not isinstance(owner.container_id, str)
                    or not _HEX_DIGEST.fullmatch(owner.container_id)
                )
            )
            or owner.checkout != self._checkout_identity
            or owner.directory.path != vm.request.directory
            or owner.manifest.path != owner.directory.path / MANIFEST_NAME
            or owner.credential_directory.path != owner.directory.path / "credentials"
            or tuple(item.path for item in owner.credential_files)
            != (
                owner.credential_directory.path / "id_ed25519",
                owner.credential_directory.path / "id_ed25519.pub",
                *(
                    (
                        owner.credential_directory.path / "host_ed25519",
                        owner.credential_directory.path / "host_ed25519.pub",
                    )
                    if len(owner.credential_files) == 4
                    else ()
                ),
            )
        ):
            raise _Denied(failure)
        self._checkout_matches(failure)
        self._outside_checkout(owner.directory.path, failure)
        owned_paths = (
            owner.checkout,
            owner.directory,
            owner.manifest,
            owner.credential_directory,
            *owner.credential_files,
        )
        opened_paths = tuple(
            handles.enter_context(_open_path(retained.path, failure)) for retained in owned_paths
        )
        for index, (retained, opened) in enumerate(zip(owned_paths, opened_paths, strict=True)):
            if opened.owned != retained:
                raise _Denied(failure)
            if index != 0:
                _private(opened, directory=index in (1, 3), failure=failure)
            if index >= 4:
                if (
                    not isinstance(retained.content_sha256, str)
                    or not _HEX_DIGEST.fullmatch(retained.content_sha256)
                    or hashlib.sha256(
                        _read_bounded(opened, _MAX_DOCUMENT_BYTES, failure)
                    ).hexdigest()
                    != retained.content_sha256
                ):
                    raise _Denied(failure)
            if index == 2:
                data = _read_bounded(opened, _MAX_DOCUMENT_BYTES, failure)
                try:
                    document = json.loads(
                        data,
                        object_pairs_hook=_unique_json_object,
                        parse_constant=_invalid_json_constant,
                    )
                except (ValueError, UnicodeError, RecursionError):
                    raise _Denied(failure) from None
                if not _same_typed_value(document, _manifest(vm)):
                    raise _Denied(failure)
            if runtime_started and index == 1:
                # Only the VM bind directory can acquire QEMU's fixed socket.
                # The executor separately bounds and validates its entry names.
                _unchanged_directory(opened, failure)
            else:
                _unchanged(opened, failure)
        return opened_paths

    def _hold_artifacts(self, vm: PreparedVm, handles: ExitStack) -> tuple[_OpenedPath, ...]:
        artifacts = vm.artifacts
        if not isinstance(artifacts, VmArtifacts):
            raise _Denied(Classification.INVALID_STATE)
        opened_paths = []
        for disk, name, gib in (
            (artifacts.os_disk, "os.qcow2", vm.request.resources.disk_gib),
            (artifacts.encrypted_disk, "encrypted.qcow2", vm.request.resources.encrypted_disk_gib),
        ):
            failure = Classification.OWNERSHIP_MISMATCH
            if (
                not isinstance(disk, DiskEvidence)
                or not _valid_owned_path(disk.file)
                or disk.file.path != vm.request.directory / name
            ):
                raise _Denied(failure)
            opened = handles.enter_context(_open_path(disk.file.path, failure))
            _admit_disk(disk, opened, gib * 1024**3)
            opened_paths.append(opened)
        seed = artifacts.seed
        failure = Classification.OWNERSHIP_MISMATCH
        if (
            not isinstance(seed, SeedEvidence)
            or not _valid_owned_path(seed.file)
            or seed.file.path != vm.request.directory / "seed.iso"
        ):
            raise _Denied(failure)
        opened = handles.enter_context(_open_path(seed.file.path, failure))
        _admit_seed(seed, opened)
        opened_paths.append(opened)
        return tuple(opened_paths)

    def _container_matches(self, vm: PreparedVm, observed: ContainerObservation | None) -> None:
        failure = Classification.OWNERSHIP_MISMATCH
        if (
            type(observed) is not ContainerObservation
            or type(observed.container_id) is not str
            or observed.container_id != vm.ownership.container_id
            or type(observed.owner_token) is not str
            or observed.owner_token != vm.ownership.token
            or not _same_typed_value(observed.mounted_directory, vm.ownership.directory.path)
            or type(observed.running) is not bool
        ):
            raise _Denied(failure)
        if observed.running:
            raise _Denied(Classification.INVALID_STATE)
        if not _local_image_id(self._runner_image_id_pin):
            raise _Denied(Classification.UNVERIFIED_IMAGE)
        assert self._runner is not None and isinstance(self._runner_image_id_pin, str)
        if not _same_typed_value(
            observed.configuration, _expected_configuration(vm.request, self._runner)
        ) or not _same_typed_value(observed.policy, _expected_policy(self._runner_image_id_pin)):
            raise _Denied(failure)

    def _container(self, vm: PreparedVm) -> ContainerObservation:
        if self._containers is None:
            raise _Denied(Classification.NATIVE_UNAVAILABLE)
        container_id = vm.ownership.container_id
        if container_id is None:
            raise _Denied(Classification.INVALID_STATE)
        try:
            observed = self._containers.inspect(container_id)
        except Exception:
            raise _Denied(Classification.NATIVE_UNAVAILABLE) from None
        if (
            not isinstance(observed, ContainerObservation)
            or observed.container_id != container_id
            or observed.owner_token != vm.ownership.token
            or observed.mounted_directory != vm.ownership.directory.path
            or type(observed.running) is not bool
        ):
            raise _Denied(Classification.OWNERSHIP_MISMATCH)
        return observed

    def prepare(self, request: PrepareRequest) -> Decision:
        try:
            self._configuration(request)
            self._checkout_matches(Classification.UNSAFE_PATH)
            self._outside_checkout(request.directory, Classification.UNSAFE_PATH)
            with (
                ExitStack() as handles,
                _open_path(request.directory, Classification.UNSAFE_PATH) as directory,
            ):
                _private(directory, directory=True, failure=Classification.UNSAFE_PATH)
                if not _empty_directory(directory.fd):
                    raise _Denied(Classification.UNSAFE_PATH)
                image = self._image(request.image, request.directory, handles)
                _unchanged(directory, Classification.UNSAFE_PATH)
                self._checkout_matches(Classification.UNSAFE_PATH)
                self._outside_checkout(request.directory, Classification.UNSAFE_PATH)
                self._image_matches(image)
                plan = VmPlan(
                    Phase.PREPARE,
                    ssh=request.ssh,
                    request=request,
                    image_evidence=image,
                    prepared_directory=directory.owned,
                    checkout=self._checkout_identity,
                    runner=self._runner,
                )
                return Decision(Phase.PREPARE, Classification.PLANNED, plan)
        except _Denied as denial:
            return Decision(Phase.PREPARE, denial.classification)
        except OSError:
            return Decision(Phase.PREPARE, Classification.NATIVE_UNAVAILABLE)

    def _lifecycle(self, phase: Phase, vm: PreparedVm) -> Decision:
        try:
            with ExitStack() as handles:
                if not isinstance(vm, PreparedVm):
                    raise _Denied(Classification.INVALID_REQUEST)
                self._configuration(vm.request)
                retained_paths = []
                if phase == Phase.START:
                    retained_paths.extend(self._hold_artifacts(vm, handles))
                retained_paths.extend(self._hold_ownership(vm, handles))
                image = None
                if phase == Phase.START:
                    if self._containers is None:
                        raise _Denied(Classification.NATIVE_UNAVAILABLE)
                    image = self._image(vm.request.image, vm.request.directory, handles)
                if vm.ownership.container_id is None:
                    if phase != Phase.START:
                        raise _Denied(Classification.INVALID_STATE)
                    # Configuration admission guarantees the native runner identity.
                    assert self._runner is not None
                    commands = (_start_command(vm, self._runner),)
                else:
                    observed = self._container(vm)
                    if image is not None:
                        self._image_matches(image)
                    container_id = vm.ownership.container_id
                    if phase == Phase.START:
                        self._container_matches(vm, observed)
                        commands = (("docker", "start", container_id),)
                    elif phase == Phase.STOP:
                        commands = (("docker", "stop", "--time", "30", container_id),)
                    else:
                        if observed.running:
                            raise _Denied(Classification.INVALID_STATE)
                        commands = (("docker", "rm", container_id),)
                # Revalidate all filesystem evidence after the last native boundary.
                for opened in retained_paths:
                    _unchanged(opened, Classification.OWNERSHIP_MISMATCH)
                self._ownership(vm)
                if image is not None:
                    with ExitStack() as final_handles:
                        self._hold_artifacts(vm, final_handles)
                    self._image_matches(image)
                plan = VmPlan(
                    phase,
                    commands,
                    vm.request.ssh if phase == Phase.START else None,
                    vm.ownership,
                    vm.ownership.directory.path if phase == Phase.REMOVE else None,
                    vm.request,
                    image,
                    checkout=self._checkout_identity,
                    runner=self._runner,
                )
                return Decision(phase, Classification.PLANNED, plan)
        except _Denied as denial:
            return Decision(phase, denial.classification)
        except OSError:
            return Decision(phase, Classification.NATIVE_UNAVAILABLE)

    def start(self, vm: PreparedVm) -> Decision:
        return self._lifecycle(Phase.START, vm)

    def stop(self, vm: PreparedVm) -> Decision:
        return self._lifecycle(Phase.STOP, vm)

    def remove(self, vm: PreparedVm) -> Decision:
        return self._lifecycle(Phase.REMOVE, vm)


class VmArgumentParser(argparse.ArgumentParser):
    """Discard argparse's input-bearing errors at the task boundary."""

    def __init__(self, phase: Phase, **kwargs):
        self._phase = phase
        super().__init__(**kwargs)

    def error(self, message):
        print(
            f"phase={self._phase.value} classification={Classification.INVALID_REQUEST.value}",
            file=sys.stderr,
        )
        raise SystemExit(2)


def run_task(
    phase: Phase,
    *,
    request: PrepareRequest | None = None,
    receipt: "Path | ExecutionReceipt | None" = None,
    executor: "VmExecutor | None" = None,
    state_directory: OwnedPath | None = None,
) -> int:
    """Execute only with independently composed native peers and held ownership.

    A decoded receipt path never grants execution. The one-shot Linux entry keeps
    original receipts in memory across provisioning, transport and teardown.
    Legacy path-only invocations remain closed rather than adopting disk records.
    """
    from spaceterm_tasks.server_vm_executor import ExecutionReceipt

    result = None
    if executor is not None:
        if phase == Phase.PREPARE and request is not None and state_directory is not None:
            result = executor.prepare(request, state_directory=state_directory)
        elif isinstance(receipt, ExecutionReceipt):
            if phase == Phase.START:
                result = executor.start(receipt)
            elif phase == Phase.STOP:
                result = executor.stop(receipt)
            elif phase == Phase.REMOVE:
                result = executor.remove(receipt)
    classification = result.classification if result is not None else Classification.GATE_NOT_READY
    print(f"phase={phase.value} classification={classification.value}")
    return (
        0
        if classification
        in (
            Classification.PREPARED,
            Classification.STARTED,
            Classification.STOPPED,
            Classification.REMOVED,
        )
        else 2
    )
