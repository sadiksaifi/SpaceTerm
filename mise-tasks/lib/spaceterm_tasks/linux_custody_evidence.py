"""Receipt-guarded Jammy transport and strict native evidence collection.

The guest peer is an injected native boundary, not execution authority. Production
composition must supply authenticated protected probes and case contracts from
native_evidence_schema. A missing observation contract refuses before guest effects.
These drivers do not implement the custody mechanisms or credit their own tests.
"""

import io
from contextlib import ExitStack
from dataclasses import dataclass, field
from typing import BinaryIO, Protocol

from spaceterm_tasks import TaskError
from spaceterm_tasks import native_evidence_schema as schema
from spaceterm_tasks import server_vm as vm
from spaceterm_tasks.server_vm_executor import ExecutionReceipt, VmExecutor

SUITES = frozenset(
    (
        "handoff",
        "memory",
        "aliases",
        "publication",
        "mount-evidence",
        "support-policy",
        "packaged-execution",
        "durability",
    )
)


@dataclass(frozen=True, slots=True)
class Candidate:
    """Independently retained comparison facts, never learned from guest output."""

    run_id: str
    revision: str
    dirty: bool
    artifact_sha256: str

    def __post_init__(self):
        schema.hexadecimal(self.run_id, 32)
        schema.hexadecimal(self.revision, 40)
        schema.boolean(self.dirty)
        schema.hexadecimal(self.artifact_sha256, 64)


@dataclass(frozen=True, slots=True)
class GuestObservation:
    container_id: str = field(repr=False)
    token: str = field(repr=False)
    administrator_uid: int = field(repr=False)
    server_uid: int = field(repr=False)
    os_version: tuple[int, int, int]
    kernel_version: tuple[int, int, int]
    libc_version: tuple[int, int, int]
    systemd_version: int
    apparmor_enabled: bool
    apport_enabled: bool
    suid_dumpable: int


def admit_guest_observation(receipt: ExecutionReceipt, value: GuestObservation) -> GuestObservation:
    if type(value) is not GuestObservation:
        raise TaskError("ownership-mismatch")
    if (
        value.container_id != receipt.container_id
        or value.token != receipt.token
        or type(value.administrator_uid) is not int
        or type(value.server_uid) is not int
        or not 0 < value.administrator_uid < 2**32 - 1
        or not 0 < value.server_uid < 2**32 - 1
        or value.administrator_uid == value.server_uid
        or not vm._same_typed_value(value.os_version, (22, 4, 0))
        or not vm._same_typed_value(value.libc_version, (2, 35, 0))
        or type(value.kernel_version) is not tuple
        or len(value.kernel_version) != 3
        or any(type(part) is not int or not 0 <= part <= 65535 for part in value.kernel_version)
        or value.kernel_version[:2] != (5, 15)
        or type(value.systemd_version) is not int
        or value.systemd_version != 249
        or value.apparmor_enabled is not True
        or value.apport_enabled is not True
        or type(value.suid_dumpable) is not int
        or value.suid_dumpable != 2
    ):
        raise TaskError("ownership-mismatch")
    return value


class GuestPeer(Protocol):
    """One already authenticated transport to the receipt-bound original guest.

    The peer admits the SSH host key and retained disposable credentials, uses
    fixed protected guest executables, and accepts no caller command or account.
    Each operation has a finite deadline. run_suite owns and settles its process
    scope before returning a bounded JSONL stream. cleanup_suite returns True only
    after observing retirement and restoring suite-local changes in the original
    guest. It must not weaken AppArmor, apport or global dumpability at install.
    """

    def observe(self) -> GuestObservation: ...

    def run_suite(self, suite: str) -> BinaryIO: ...

    def cleanup_suite(self, suite: str) -> bool: ...


class CustodyDriver:
    """Hold independent VM namespace authority through transport and cleanup.

    Required cases come from the authenticated suite composition, not downloaded
    JSONL. The shared schema must define every observed effect before execution.
    No unavailable, synthetic, skipped or partial probe run satisfies collection.
    A Collection confirms only these required probes. It is not whole-suite credit;
    the complete Linux behavior contracts and native peer must be composed first.
    """

    def __init__(self, executor: VmExecutor, peer: GuestPeer, candidate: Candidate):
        self.executor, self.peer, self.candidate = executor, peer, candidate

    def _observe(self, receipt: ExecutionReceipt) -> GuestObservation:
        return admit_guest_observation(receipt, self.peer.observe())

    def _collect(
        self,
        stream: BinaryIO,
        required: tuple[schema.RequiredCase, ...],
        observed: GuestObservation,
        sink: BinaryIO | None,
    ) -> schema.Collection:
        collector = schema.Collector()
        while line := stream.readline(schema.RECORD_BYTES + 1):
            if (
                type(line) is not bytes
                or not line.endswith(b"\n")
                or len(line) > schema.RECORD_BYTES
            ):
                raise TaskError("invalid-record")
            value = schema.decode_json(line)
            schema.validate_record(value)
            host = value["host"]
            if (
                value["run_id"] != self.candidate.run_id
                or value["revision"] != self.candidate.revision
                or value["dirty"] is not self.candidate.dirty
                or value["artifact_sha256"] != self.candidate.artifact_sha256
                or value["proof"] != "native"
                or value["context"] != "disposable-jammy"
                or host["platform"] != "linux"
                or host["architecture"] != "x86_64"
                or tuple(host["os_version"]) != observed.os_version
                or tuple(host["kernel_version"]) != observed.kernel_version
                or tuple(host["libc_version"]) != observed.libc_version
                or host["supervisor"] != "systemd"
                or host["supervisor_version"][0] != 249
            ):
                raise TaskError("invalid-record")
            collector.append(schema.encode_record(value), sink)
        return collector.finish(list(required))

    def collect_probe(
        self,
        receipt: ExecutionReceipt,
        suite: str,
        required: tuple[schema.RequiredCase, ...],
        *,
        sink: BinaryIO | None = None,
    ) -> schema.Collection:
        if (
            type(suite) is not str
            or suite not in SUITES
            or type(required) is not tuple
            or not required
            or len(required) > schema.RUN_RECORDS
            or any(
                type(case) is not schema.RequiredCase
                or case.context != "disposable-jammy"
                or case.result not in ("passed", "denied")
                for case in required
            )
            or len(set(required)) != len(required)
        ):
            raise TaskError("invalid-request")
        if any(
            schema.CASE_FACTS.get((case.job, case.case)) in (None, "privilege") for case in required
        ):
            # The existing privilege facts explicitly prove only macOS. Linux
            # needs filesystem IDs, capabilities and continuous nondumpability.
            raise TaskError("native-unavailable")
        with ExitStack() as handles:
            try:
                # This driver and the executor share one receipt owner. Holding
                # both locks excludes concurrent Stop/remove throughout the run.
                execution = self.executor._admit_cleanup(receipt, handles, "started")
                if not self.executor._runtime_observation(execution).running:
                    raise TaskError("invalid-state")
                observed = self._observe(receipt)
                execution.check(runtime_started=True)
            except vm._Denied as error:
                raise TaskError(error.classification.value) from None
            except TaskError as error:
                if str(error) in schema.FAILURES or str(error) in {
                    kind.value for kind in vm.Classification
                }:
                    raise
                raise TaskError("native-unavailable") from None
            except Exception:
                raise TaskError("native-unavailable") from None
            error = None
            collected = None
            buffered = io.BytesIO()
            try:
                with self.peer.run_suite(suite) as stream:
                    collected = self._collect(stream, required, observed, buffered)
            except BaseException as failure:
                error = failure
            try:
                # A replaced guest is not a cleanup target. The retained receipt
                # remains available for reviewed recovery of the original scope.
                if self._observe(receipt) != observed:
                    raise TaskError("cleanup-unresolved")
                execution.check(runtime_started=True)
                if self.peer.cleanup_suite(suite) is not True:
                    raise TaskError("cleanup-unresolved")
                if self._observe(receipt) != observed:
                    raise TaskError("cleanup-unresolved")
                execution.check(runtime_started=True)
            except BaseException:
                raise TaskError("cleanup-unresolved") from None
            if error is not None:
                if isinstance(error, (KeyboardInterrupt, SystemExit)):
                    raise error
                if isinstance(error, TaskError) and str(error) in schema.FAILURES:
                    raise error
                raise TaskError("native-unavailable") from None
            assert collected is not None
            # Do not publish decisive records before cleanup has been observed.
            if sink is not None:
                data = buffered.getvalue()
                try:
                    if sink.write(data) != len(data):
                        raise TaskError("io-unavailable")
                except Exception:
                    raise TaskError("io-unavailable") from None
            return collected
