#!/usr/bin/env -S python -I
# MISE description="Collect bounded native records from the same signed candidate"
"""Keep partial observations. Never turn owner checkpoints into native evidence."""

import io
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks/lib"))
sys.path.insert(0, str(ROOT / "packaging/macos/native-evidence"))

from assembly import verify_candidate  # noqa: E402  # ty: ignore[unresolved-import]
from observations import (  # noqa: E402  # ty: ignore[unresolved-import]
    native_host,
    read_input,
    write_wire_record,
)
from sequence import (  # noqa: E402  # ty: ignore[unresolved-import]
    default_state,
    read_outcomes,
    read_preflight,
    step_done,
)
from spaceterm_tasks import TaskError  # noqa: E402
from spaceterm_tasks.native_evidence_schema import (  # noqa: E402
    CASE_FACTS,
    RUN_BYTES,
    RUN_RECORDS,
    Collector,
    RequiredCase,
    decode_json,
    encode_record,
    validate_record,
)
from spaceterm_tasks.server_native_macos import (  # noqa: E402
    EvidenceArgumentParser,
)
from spaceterm_tasks.server_native_macos import (  # noqa: E402
    required_cases as legacy_required_cases,
)

PUBLICATION_CASE = "publication-authenticates-protected-copies-before-root-exec"


def required_cases(profile):
    # The administrator was dropped, not protected-copy authentication. These
    # retained owner cases stay incomplete until their native CASE_FACTS exist.
    context = "gui-parented" if profile == "local" else "disposable-macos"
    daemon = "daemon-parented" if profile == "local" else "disposable-macos"
    cases = [
        case
        for case in legacy_required_cases(profile)
        if case.case != "bootstrap-authenticates-protected-admin-before-root-exec"
    ]
    cases.extend(
        RequiredCase("native-installation", case, daemon)
        for case in (
            "boot-before-login",
            "running-survives-logout",
            "stopped-survives-reboot",
            "storage-unlock-recovery",
        )
    )
    cases.append(RequiredCase("macos-management", "background-items-suppression", context))
    if ("macos-evidence", PUBLICATION_CASE) in CASE_FACTS:
        cases.append(RequiredCase("macos-evidence", PUBLICATION_CASE, context))
    return cases


@dataclass(frozen=True, slots=True)
class EvidenceCollection:
    records: int
    bytes: int
    run_id: str
    passed: int
    unrun: int
    unresolved: int
    retention: str


def read_parts(inputs, identity, phase):
    collector = Collector()
    records = {}
    if not inputs or len(inputs) > 4096:
        raise TaskError("invalid-record")
    for path in inputs:
        data = read_input(path, 32 * 1024 * 1024)
        for sequence, line in enumerate(io.BytesIO(data), start=1):
            record = decode_json(line)
            validate_record(record)
            if record["sequence"] != sequence:
                raise TaskError("invalid-record")
            if identity is not None:
                expected = {
                    "run_id": identity["run_id"],
                    "artifact_sha256": identity["artifact_sha256"],
                    "revision": identity.get(f"{phase}_revision", identity["revision"]),
                    "dirty": identity.get(f"{phase}_dirty", identity["dirty"]),
                }
                if any(record[field] != value for field, value in expected.items()):
                    raise TaskError("identity-mismatch")
            record["sequence"] = collector.records + 1
            collector.append(encode_record(record))
            records[(record["job"], record["case"], record["context"], record["proof"])] = record
    return collector, records


def collect_inputs(
    inputs,
    identity,
    *,
    partial=False,
    profile="local",
    after_update_inputs=None,
    owner_outcomes=None,
):
    after_update_inputs = after_update_inputs or []
    if len(inputs) + len(after_update_inputs) > 4096:
        raise TaskError("limit-exceeded")
    if after_update_inputs:
        if identity is None or owner_outcomes is None:
            raise TaskError("invalid-record")
        notes = read_outcomes(owner_outcomes, identity)
        if not step_done(notes, 10):
            raise TaskError("outcome-pending")
    baseline, before = read_parts(inputs, identity, "baseline")
    summaries = [baseline.finish([])]
    after = {}
    if after_update_inputs:
        updated, after = read_parts(after_update_inputs, identity, "update")
        summaries.append(updated.finish([]))
        if next(iter(before.values()))["host"] != next(iter(after.values()))["host"]:
            raise TaskError("identity-mismatch")
    if (
        sum(summary.records for summary in summaries) > RUN_RECORDS
        or sum(summary.bytes for summary in summaries) > RUN_BYTES
    ):
        raise TaskError("limit-exceeded")
    retention = "unrun"
    retention_unresolved = 0
    if after_update_inputs:
        # Desktop effects are comparison evidence, not server-grant retention.
        # Native producers must bind each daemon effect to the phase's exact
        # server execution. Revisions and the pair digest were checked above.
        grants = {
            key
            for key, record in before.items()
            if key[0] == "macos-capabilities"
            and key[1]
            in (
                "protected-folders",
                "selected-volume",
                "microphone",
                "screen-recording",
                "accessibility",
                "apple-events",
            )
            and key[2:] == ("daemon-parented", "native")
            and record["result"] == "passed"
        }
        activation = after.get(
            ("macos-management", "fresh-signed-update", "daemon-parented", "native")
        )
        retention = (
            "passed"
            if grants
            and activation is not None
            and activation["result"] == "passed"
            and all(
                before[key]["facts"]["responsible_process"] == "server"
                and key in after
                and after[key]["result"] == "passed"
                and after[key]["facts"]["responsible_process"] == "server"
                for key in grants
            )
            else "pending"
        )
        retention_unresolved = int(retention != "passed")
    # E2 owns the publication observation contract. Until its decisive facts
    # are integrated, final collection cannot succeed by deleting the old case.
    missing = int(not partial and ("macos-evidence", PUBLICATION_CASE) not in CASE_FACTS)
    if not partial:
        for requirement in required_cases(profile):
            key = (requirement.job, requirement.case, requirement.context, "native")
            # The initial capability matrix is retained. Native management facts
            # after activation are attributed to the update, not relabelled.
            candidates = [before.get(key)]
            if requirement.job != "macos-capabilities":
                candidates.append(after.get(key))
            decisive = (
                ("passed", "denied", "cancelled", "unavailable", "timed-out")
                if requirement.result is None
                else (requirement.result,)
            )
            if not any(
                record is not None and record["result"] in decisive for record in candidates
            ):
                missing += 1
        if not after_update_inputs:
            retention_unresolved = 1
    return EvidenceCollection(
        sum(summary.records for summary in summaries),
        sum(summary.bytes for summary in summaries),
        summaries[0].run_id,
        sum(summary.passed for summary in summaries),
        sum(summary.unrun for summary in summaries) + missing,
        sum(summary.unresolved for summary in summaries) + retention_unresolved,
        retention,
    )


from assembly import task_main  # noqa: E402  # ty: ignore[unresolved-import]


def entry():
    parser = EvidenceArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path)
    parser.add_argument("--parts", type=Path)
    parser.add_argument("--after-update-parts", type=Path)
    parser.add_argument("--state", type=Path)
    parser.add_argument("--profile", choices=("local", "disposable"), default="local")
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--partial", action="store_true")
    parser.add_argument("--observation", action="store_true")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    if (arguments.input is None) == (arguments.parts is None):
        raise TaskError("invalid-record")
    identity = None
    if arguments.candidate is not None:
        identity = read_preflight(arguments.candidate)
        for name in ("baseline", "update"):
            verify_candidate(arguments.candidate / name)
    if arguments.observation:
        if (
            arguments.input is None
            or arguments.output is None
            or identity is None
            or arguments.partial
            or arguments.after_update_parts is not None
            or arguments.state is not None
        ):
            raise TaskError("invalid-record")
        record = write_wire_record(
            arguments.output, read_input(arguments.input, 32), identity, native_host()
        )
        print(f"phase=observation classification={record['result']} records=1")
        return (
            2
            if record["result"]
            in ("pending", "indeterminate", "cleanup-unresolved", "failed", "unrun")
            else 0
        )
    if arguments.output is not None:
        raise TaskError("invalid-record")
    if arguments.parts is not None:
        if identity is None or not arguments.parts.is_dir() or arguments.parts.is_symlink():
            raise TaskError("invalid-record")
        inputs = sorted(arguments.parts.glob("*.jsonl"))
    else:
        inputs = [arguments.input]
    after_inputs = []
    state = None
    if arguments.after_update_parts is not None:
        if (
            identity is None
            or arguments.parts is None
            or not arguments.after_update_parts.is_dir()
            or arguments.after_update_parts.is_symlink()
            or arguments.after_update_parts == arguments.parts
        ):
            raise TaskError("invalid-record")
        after_inputs = sorted(arguments.after_update_parts.glob("*.jsonl"))
        if not after_inputs:
            raise TaskError("invalid-record")
        state = arguments.state or default_state(identity["run_id"])
    elif arguments.state is not None:
        raise TaskError("invalid-record")
    try:
        summary = collect_inputs(
            inputs,
            identity,
            partial=arguments.partial,
            profile=arguments.profile,
            after_update_inputs=after_inputs,
            owner_outcomes=state,
        )
    except TaskError as error:
        if str(error) != "outcome-pending":
            raise
        print("classification=incomplete")
        return 2
    classification = (
        "incomplete"
        if arguments.partial or summary.unrun or summary.unresolved
        else "records-valid"
    )
    print(
        f"classification={classification} records={summary.records} native_passed={summary.passed} unrun={summary.unrun} unresolved={summary.unresolved} update_retention={summary.retention}"
    )
    return 2 if arguments.partial or summary.unrun or summary.unresolved else 0


if __name__ == "__main__":
    task_main(entry)
