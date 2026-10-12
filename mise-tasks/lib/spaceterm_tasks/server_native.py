"""Read-only admission policy for the native server feasibility gate.

READY means an inspector reported consistent registration, storage, platform, and
supervisor prerequisites. It does not verify boot, logout, credential access,
account isolation, or privacy grants. Inspection never prepares or changes host integration.
"""

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


def run_task(phase: Phase = Phase.INSPECT) -> int:
    """The command has no native inspector and cannot claim host evidence."""
    evidence = NativeFeasibility().run(phase)
    print(f"phase={evidence.phase.value} classification={evidence.classification.value}")
    return 2
