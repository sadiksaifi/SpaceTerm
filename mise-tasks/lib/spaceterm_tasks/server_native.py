"""Read-only admission policy for the native server feasibility gate.

READY means an inspector reported consistent registration, storage, platform, and
supervisor prerequisites. It does not verify boot, logout, credential access,
account isolation, or privacy grants. Inspection never prepares or changes host integration.
"""

import os
import platform
import stat
from dataclasses import dataclass, field
from enum import StrEnum
from typing import Protocol

from spaceterm_tasks import TaskError


class Namespace(StrEnum):
    DEVELOPMENT = "io.github.sadiksaifi.spaceterm-development"
    PREFLIGHT = "io.github.sadiksaifi.spaceterm-preflight"
    RELEASE = "io.github.sadiksaifi.spaceterm"

    @property
    def server_identifier(self) -> str:
        return f"{self.value}.server"


class Phase(StrEnum):
    PREPARE = "prepare"
    INSPECT = "inspect"
    INSTALL = "install"
    EXERCISE = "exercise"
    CLEANUP = "cleanup"


class Registration(StrEnum):
    UNCHECKED = "unchecked"
    MISSING = "missing"
    PRESENT = "present"
    DENIED = "denied"
    UNAVAILABLE = "unavailable"


class DesiredState(StrEnum):
    UNKNOWN = "unknown"
    RUNNING = "running"
    STOPPED = "stopped"


class Storage(StrEnum):
    UNCHECKED = "unchecked"
    AVAILABLE = "available"
    LOCKED = "locked"
    DENIED = "denied"
    UNAVAILABLE = "unavailable"


class HostPlatform(StrEnum):
    UNKNOWN = "unknown"
    MACOS = "macos"
    LINUX = "linux"
    UNSUPPORTED = "unsupported"


class Supervisor(StrEnum):
    UNKNOWN = "unknown"
    LAUNCHD = "launchd"
    SYSTEMD = "systemd"
    MISSING = "missing"


class Classification(StrEnum):
    GATE_NOT_READY = "gate-not-ready"
    READY = "ready"
    MISSING_INSTALLATION = "missing-installation"
    STOPPED = "stopped"
    STORAGE_LOCKED = "storage-locked"
    STORAGE_UNAVAILABLE = "storage-unavailable"
    INSPECTION_DENIED = "inspection-denied"
    INSPECTION_UNAVAILABLE = "inspection-unavailable"
    UNSUPPORTED_HOST = "unsupported-host"
    INVALID_EVIDENCE = "invalid-evidence"


@dataclass(frozen=True, slots=True)
class Version:
    major: int
    minor: int = 0
    patch: int = 0

    def __post_init__(self):
        if any(
            type(value) is not int or not 0 <= value <= 65535
            for value in (self.major, self.minor, self.patch)
        ):
            raise TaskError(Classification.INVALID_EVIDENCE.value)


@dataclass(frozen=True, slots=True)
class HostFacts:
    platform: HostPlatform = HostPlatform.UNKNOWN
    os_version: Version | None = None
    supervisor: Supervisor = Supervisor.UNKNOWN
    supervisor_version: Version | None = None
    credential_storage: Storage = Storage.UNCHECKED

    def __post_init__(self):
        if (
            type(self.platform) is not HostPlatform
            or type(self.supervisor) is not Supervisor
            or type(self.credential_storage) is not Storage
            or (self.os_version is not None and type(self.os_version) is not Version)
            or (
                self.supervisor_version is not None and type(self.supervisor_version) is not Version
            )
            or (self.platform is HostPlatform.MACOS and self.supervisor is Supervisor.SYSTEMD)
            or (self.platform is HostPlatform.LINUX and self.supervisor is Supervisor.LAUNCHD)
            or (
                self.supervisor in (Supervisor.UNKNOWN, Supervisor.MISSING)
                and self.supervisor_version is not None
            )
        ):
            raise TaskError(Classification.INVALID_EVIDENCE.value)


@dataclass(frozen=True, slots=True)
class NativeObservation:
    """Native facts, never paths, credential values, native messages, or command output.

    Registration absence requires an affirmative native observation. Only a present
    registration supplies desired state and application storage facts. Locked or
    unavailable storage does not imply missing registration. Reading credential_storage
    reports availability without retrieving or unlocking a key.
    """

    registration: Registration
    desired_state: DesiredState
    application_storage: Storage
    host: HostFacts = field(default_factory=HostFacts)

    def __post_init__(self):
        if (
            type(self.registration) is not Registration
            or type(self.desired_state) is not DesiredState
            or type(self.application_storage) is not Storage
            or type(self.host) is not HostFacts
            or (
                self.registration is Registration.PRESENT
                and self.desired_state is DesiredState.UNKNOWN
            )
            or (
                self.registration is not Registration.PRESENT
                and (
                    self.desired_state is not DesiredState.UNKNOWN
                    or self.application_storage is not Storage.UNCHECKED
                )
            )
        ):
            raise TaskError(Classification.INVALID_EVIDENCE.value)


class NativeInspector(Protocol):
    def observe(self, namespace: Namespace) -> NativeObservation:
        """Read status without key access, prompts, repairs, or host changes.

        Read registered desired state before storage probes. Stopped registrations
        can leave storage unchecked; inspection must not retrieve or unlock a key.
        """
        ...


@dataclass(frozen=True, slots=True)
class Evidence:
    phase: Phase
    namespace: Namespace
    classification: Classification
    observation: NativeObservation | None = None

    def __post_init__(self):
        if (
            type(self.phase) is not Phase
            or type(self.namespace) is not Namespace
            or type(self.classification) is not Classification
            or (self.observation is not None and type(self.observation) is not NativeObservation)
        ):
            raise TaskError(Classification.INVALID_EVIDENCE.value)


class NativeFeasibility:
    """Own phase admission and classification; inject native read-only inspection.

    Default execution means inspection, never setup or repair. No Boolean flag or
    paired-device proof authorizes host changes. Until a phase's adapter and native
    authorization contract exist, that phase reports gate-not-ready without effects.
    """

    def __init__(
        self,
        inspector: NativeInspector | None = None,
        namespace: Namespace = Namespace.DEVELOPMENT,
    ):
        if type(namespace) is not Namespace:
            raise TaskError(Classification.INVALID_EVIDENCE.value)
        self._inspector = inspector
        self._namespace = namespace

    def run(self, phase: Phase = Phase.INSPECT) -> Evidence:
        if type(phase) is not Phase:
            raise TaskError(Classification.INVALID_EVIDENCE.value)
        if phase is not Phase.INSPECT or self._inspector is None:
            return Evidence(phase, self._namespace, Classification.GATE_NOT_READY)

        try:
            observation = self._inspector.observe(self._namespace)
        except PermissionError:
            return Evidence(phase, self._namespace, Classification.INSPECTION_DENIED)
        except Exception:
            # Native adapters can raise content-bearing failures outside OSError.
            return Evidence(phase, self._namespace, Classification.INSPECTION_UNAVAILABLE)

        if type(observation) is not NativeObservation:
            raise TaskError(Classification.INVALID_EVIDENCE.value)
        return Evidence(phase, self._namespace, self._classify(observation), observation)

    @staticmethod
    def _classify(observation: NativeObservation) -> Classification:
        match observation.registration:
            case Registration.UNCHECKED:
                return Classification.GATE_NOT_READY
            case Registration.MISSING:
                return Classification.MISSING_INSTALLATION
            case Registration.DENIED:
                return Classification.INSPECTION_DENIED
            case Registration.UNAVAILABLE:
                return Classification.INSPECTION_UNAVAILABLE

        # Deliberate Stop takes precedence over storage and host readiness.
        if observation.desired_state is DesiredState.STOPPED:
            return Classification.STOPPED

        host = observation.host
        storage = (observation.application_storage, host.credential_storage)
        if Storage.DENIED in storage:
            return Classification.INSPECTION_DENIED
        if Storage.LOCKED in storage:
            return Classification.STORAGE_LOCKED
        if Storage.UNAVAILABLE in storage:
            return Classification.STORAGE_UNAVAILABLE
        if host.platform is HostPlatform.UNSUPPORTED or host.supervisor is Supervisor.MISSING:
            return Classification.UNSUPPORTED_HOST
        if (
            host.platform is HostPlatform.LINUX
            and host.supervisor_version is not None
            and host.supervisor_version.major < 249
        ):
            return Classification.UNSUPPORTED_HOST
        if (
            Storage.UNCHECKED in storage
            or host.platform is HostPlatform.UNKNOWN
            or host.supervisor is Supervisor.UNKNOWN
            or (host.platform is HostPlatform.LINUX and host.supervisor_version is None)
        ):
            return Classification.GATE_NOT_READY
        return Classification.READY


class RegistrationStatusReader(Protocol):
    def read(self, namespace: Namespace, uid: int) -> bytes | None: ...


class FixedMacOSRegistrationReader:
    """Read bounded candidates at the fixed root without creating missing storage.

    Directory-relative opening rejects links and obvious account-write exposure.
    These observations are not a custody lease or startup admission. The native
    authority remains the only owner of schema-2 profile and storage validation.
    """

    def read(self, namespace: Namespace, uid: int) -> bytes | None:
        descriptors = []
        try:
            descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            descriptors.append(descriptor)
            for name in ("Library", "Application Support", "SpaceTerm", "ServerRegistrations"):
                info = os.fstat(descriptor)
                if info.st_uid != 0 or info.st_mode & 0o022:
                    raise PermissionError()
                try:
                    descriptor = os.open(
                        name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor
                    )
                except FileNotFoundError:
                    return None
                descriptors.append(descriptor)
            build = {
                Namespace.DEVELOPMENT: "development",
                Namespace.PREFLIGHT: "preflight",
                Namespace.RELEASE: "release",
            }[namespace]
            info = os.fstat(descriptor)
            if info.st_uid != 0 or info.st_mode & 0o022:
                raise PermissionError()
            try:
                descriptor = os.open(
                    f"{build}-{uid}.registration",
                    os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                    dir_fd=descriptor,
                )
            except FileNotFoundError:
                return None
            descriptors.append(descriptor)
            info = os.fstat(descriptor)
            if (
                not stat.S_ISREG(info.st_mode)
                or info.st_uid != 0
                or info.st_mode & 0o022
                or info.st_nlink != 1
            ):
                raise PermissionError()
            data = bytearray()
            while len(data) <= 256:
                chunk = os.read(descriptor, 257 - len(data))
                if not chunk:
                    break
                data.extend(chunk)
            current = os.fstat(descriptor)
            fields = (
                "st_dev",
                "st_ino",
                "st_mode",
                "st_uid",
                "st_gid",
                "st_nlink",
                "st_size",
                "st_mtime_ns",
                "st_ctime_ns",
            )
            if any(getattr(current, field) != getattr(info, field) for field in fields):
                raise OSError()
            return bytes(data)
        finally:
            for descriptor in reversed(descriptors):
                os.close(descriptor)


class MacOSReadOnlyInspector:
    """Observe registration and host facts. Never open any credential facility.

    Running leaves storage unchecked. Schema-2 interpretation requires the native
    authority's read-only status binding and is unavailable until it is integrated.
    No Python pathname check substitutes for that authority.
    """

    def __init__(
        self,
        reader: RegistrationStatusReader | None = None,
        host: HostFacts | None = None,
        uid: int | None = None,
    ):
        self._reader = reader if reader is not None else FixedMacOSRegistrationReader()
        self._host = host
        self._uid = os.getuid() if uid is None else uid

    def observe(self, namespace: Namespace) -> NativeObservation:
        if type(self._uid) is not int or not 0 < self._uid < 2**32 - 1:
            raise TaskError("identity-mismatch")
        host = self._host
        if host is None:
            host = HostFacts(platform=HostPlatform.UNSUPPORTED)
            if platform.system() == "Darwin" and platform.machine() == "arm64":
                # platform.mac_ver reads the system version plist, not a Keychain.
                parts = platform.mac_ver()[0].split(".")
                if not 1 <= len(parts) <= 3 or any(not part.isdecimal() for part in parts):
                    raise TaskError("native-unavailable")
                values = [int(part) for part in parts] + [0] * (3 - len(parts))
                host = HostFacts(
                    platform=HostPlatform.MACOS,
                    os_version=Version(*values),
                    supervisor=Supervisor.LAUNCHD,
                )
            else:
                return NativeObservation(
                    Registration.UNAVAILABLE, DesiredState.UNKNOWN, Storage.UNCHECKED, host
                )
        data = self._reader.read(namespace, self._uid)
        if data is None:
            return NativeObservation(
                Registration.MISSING, DesiredState.UNKNOWN, Storage.UNCHECKED, host
            )
        build = {Namespace.DEVELOPMENT: 3, Namespace.PREFLIGHT: 2, Namespace.RELEASE: 1}[namespace]
        if (
            type(data) is not bytes
            or len(data) != 48
            or data[:10] != b"STRGREC\0\x01\x00"
            or data[10] != build
            or data[11] not in (1, 2)
            or int.from_bytes(data[12:16], "little") != self._uid
            or not int.from_bytes(data[16:24], "little")
            or not int.from_bytes(data[24:32], "little")
            or any(data[40:48])
        ):
            raise TaskError("native-unavailable")
        state = DesiredState.STOPPED if data[11] == 2 else DesiredState.RUNNING
        return NativeObservation(Registration.PRESENT, state, Storage.UNCHECKED, host)


def run_task(phase: Phase = Phase.INSPECT) -> int:
    """Default inspection reads candidates only and never claims startup evidence."""
    evidence = NativeFeasibility(MacOSReadOnlyInspector()).run(phase)
    print(f"phase={evidence.phase.value} classification={evidence.classification.value}")
    return (
        0
        if evidence.classification in (Classification.MISSING_INSTALLATION, Classification.STOPPED)
        else 2
    )
