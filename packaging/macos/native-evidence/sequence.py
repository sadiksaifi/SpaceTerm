"""One local owner sequence. Checkpoints are operator notes, never native proof."""

import fcntl
import hashlib
import os
import stat
from pathlib import Path

from assembly import (
    DEFAULT_PREPARED,
    MANIFEST,
    copy_candidate,
    new_directory,
    require_update_pair,
    verify_candidate,
)
from spaceterm_tasks import TaskError, package_server
from spaceterm_tasks.native_evidence_schema import decode_json
from spaceterm_tasks.server_native_macos import canonical_json, read_regular

# Owner steps 8 and 14 are one suppression/recovery interaction, recorded once.
STEPS = ((1,), (2,), (3,), (4,), (12,), (13,), (8, 14), (9,), (10,), (5,), (6,), (7,), (11,))
OUTCOMES = ("done", "denied", "cancelled", "unavailable", "cleanup-unresolved")
AUTHENTICATION = ("touch-id", "password", "none")


def default_state(run_id):
    return (
        Path.home()
        / "Library/Application Support/SpaceTerm Native Evidence 520"
        / run_id
        / "owner-outcomes.jsonl"
    )


def effective_outcomes(records):
    outcomes = []
    for record in records:
        if record["scope"] == "coordinator-resolution":
            outcomes[-1] = {**outcomes[-1], "outcome": "done"}
        elif outcomes and record["owner_steps"] == [1]:
            outcomes[0] = record
        else:
            outcomes.append(record)
    return outcomes


def checkpoint_status(records):
    outcomes = effective_outcomes(records)
    if not outcomes:
        return "ready"
    if outcomes[-1]["outcome"] == "cleanup-unresolved":
        return "recovery-required"
    if outcomes[0]["outcome"] != "done":
        if len(records) == 1 and outcomes[0]["outcome"] in ("denied", "cancelled"):
            # Install denial/cancellation is zero-effect. Retain the refusal and
            # allow one real retry, never a resolution pretending it installed.
            return "retry-install"
        return "run-ended"
    return "ready"


def halted(records):
    return checkpoint_status(records) in ("recovery-required", "run-ended")


def checkpoint_index(records):
    return len(effective_outcomes(records)) - int(checkpoint_status(records) == "retry-install")


def valid_authentication(record):
    if record["owner_steps"] != [13] or record["scope"] == "coordinator-resolution":
        return "authentication" not in record
    methods = record.get("authentication")
    return (
        isinstance(methods, dict)
        and set(methods) == {"gui-parented", "daemon-parented"}
        and all(method in AUTHENTICATION for method in methods.values())
        and (record["outcome"] != "done" or "none" not in methods.values())
    )


def step_done(records, step):
    return not halted(records) and any(
        record["owner_steps"][0] == step and record["outcome"] == "done"
        for record in effective_outcomes(records)
    )


def pair_identity(root):
    before, after = require_update_pair(root / "baseline", root / "update")
    digest = hashlib.sha256()
    for name in ("baseline", "update"):
        digest.update(read_regular(root / name, MANIFEST, 1024 * 1024))
    return {
        "run_id": before["run_id"],
        "artifact_sha256": digest.hexdigest(),
        "revision": before["revision"],
        "dirty": before["dirty"],
        "baseline_revision": before["revision"],
        "baseline_dirty": before["dirty"],
        "update_revision": after["revision"],
        "update_dirty": after["dirty"],
        "baseline_version": before["metadata"]["version"],
        "update_version": after["metadata"]["version"],
    }


def prepare_pair(source, destination=DEFAULT_PREPARED, *, verify=verify_candidate):
    require_update_pair(source / "baseline", source / "update")
    root = new_directory(destination)
    for name in ("baseline", "update"):
        copy_candidate(source / name, root / name)
        verify(root / name)
        manifest = package_server.verify_archive(source / f"{name}.tar.gz")
        installation = decode_json(
            read_regular(root / name, "server/installation.json", 1024 * 1024), 1024 * 1024
        )
        if manifest != installation:
            raise TaskError("artifact-rejected")
        # Rebuild a deterministic archive from the verified, copied package. No
        # downloaded archive is extracted or used as an interpreter directory.
        package_server.write_archive(root / name / "server", root / f"{name}.tar.gz")
        package_server.verify_archive(root / f"{name}.tar.gz")
    identity = pair_identity(root)
    preflight = {
        "schema": 2,
        "scope": "native-evidence-520-preflight",
        **identity,
        "signatures_verified": True,
        "native_passed": 0,
        "production_admission": "uncredited",
        "independent_review": "required",
        "protected_publication": "required-before-execution",
    }
    (root / "preflight.json").write_bytes(canonical_json(preflight))
    for name in ("local-sequence.md", "jammy-sequence.md"):
        text = (Path(__file__).parent / name).read_text()
        for token, value in {
            "@CANDIDATE@": str(root),
            "@RUN_ID@": identity["run_id"],
            "@PAIR_DIGEST@": identity["artifact_sha256"],
            "@CHECKOUT@": str(Path(__file__).resolve().parents[3]),
            "@STATE@": str(default_state(identity["run_id"]).parent),
        }.items():
            text = text.replace(token, value)
        (root / name).write_text(text)
    return preflight


def read_preflight(root):
    document = decode_json(read_regular(root, "preflight.json", 8192), 8192)
    expected = {
        "schema": 2,
        "scope": "native-evidence-520-preflight",
        **pair_identity(root),
        "signatures_verified": True,
        "native_passed": 0,
        "production_admission": "uncredited",
        "independent_review": "required",
        "protected_publication": "required-before-execution",
    }
    if document != expected:
        raise TaskError("artifact-rejected")
    return document


def read_outcomes(file, identity):
    if not file.exists() and not file.is_symlink():
        return []
    descriptor = os.open(file, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 16384:
            raise TaskError("invalid-record")
        records = []
        for line in source:
            record = decode_json(line, 1024)
            resolution = (
                isinstance(record, dict) and record.get("scope") == "coordinator-resolution"
            )
            index = checkpoint_index(records) - int(resolution)
            if (
                index < 0
                or index >= len(STEPS)
                or not isinstance(record, dict)
                or set(record)
                != {
                    "schema",
                    "scope",
                    "sequence",
                    "run_id",
                    "artifact_sha256",
                    "owner_steps",
                    "outcome",
                }
                | ({"authentication"} if not resolution and STEPS[index] == (13,) else set())
                or record["schema"] != 1
                or type(record["schema"]) is not int
                or record["scope"] not in ("owner-checkpoint", "coordinator-resolution")
                or type(record["sequence"]) is not int
                or record["sequence"] != len(records) + 1
                or record["owner_steps"] != list(STEPS[index])
                or record["run_id"] != identity["run_id"]
                or record["artifact_sha256"] != identity["artifact_sha256"]
                or record["outcome"] not in OUTCOMES
                or not valid_authentication(record)
                or (
                    resolution
                    and (
                        checkpoint_status(records) != "recovery-required"
                        or record["outcome"] != "done"
                    )
                )
                or (not resolution and halted(records))
            ):
                raise TaskError("invalid-record")
            records.append(record)
        after = os.fstat(descriptor)
        if (info.st_size, info.st_mtime_ns, info.st_ctime_ns) != (
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ):
            raise TaskError("invalid-record")
    return records


def checkpoint(file, identity, step=None, outcome=None, *, resolve=False, authentication=None):
    # A small account-owned file serializes owner notes only. It supplies no
    # publication, native identity, credential release or cleanup authority.
    file.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = os.open(file, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        info = os.fstat(descriptor)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_nlink != 1
            or info.st_uid != os.getuid()
            or info.st_mode & 0o077
        ):
            raise TaskError("unsafe-native-state")
        records = read_outcomes(file, identity)
        progress = checkpoint_index(records)
        if resolve:
            if (
                checkpoint_status(records) != "recovery-required"
                or outcome != "done"
                or step != STEPS[progress - 1][0]
            ):
                raise TaskError("invalid-record")
            owner_steps = STEPS[progress - 1]
        else:
            if halted(records):
                if step is not None:
                    raise TaskError("outcome-pending")
                return None
            if step is None:
                return STEPS[progress] if progress < len(STEPS) else ()
            if outcome not in OUTCOMES or progress >= len(STEPS) or step != STEPS[progress][0]:
                raise TaskError("invalid-record")
            owner_steps = STEPS[progress]
        record = {
            "schema": 1,
            "scope": "coordinator-resolution" if resolve else "owner-checkpoint",
            "sequence": len(records) + 1,
            "run_id": identity["run_id"],
            "artifact_sha256": identity["artifact_sha256"],
            "owner_steps": list(owner_steps),
            "outcome": outcome,
        }
        if authentication is not None:
            if not isinstance(authentication, tuple) or len(authentication) != 2:
                raise TaskError("invalid-record")
            record["authentication"] = dict(
                zip(("gui-parented", "daemon-parented"), authentication, strict=True)
            )
        if not valid_authentication(record):
            raise TaskError("invalid-record")
        encoded = canonical_json(record) + b"\n"
        os.lseek(descriptor, 0, os.SEEK_END)
        while encoded:
            count = os.write(descriptor, encoded)
            if count <= 0:
                raise TaskError("io-unavailable")
            encoded = encoded[count:]
        os.fsync(descriptor)
        parent = os.open(file.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
        records.append(record)
        if halted(records):
            return None
        progress = checkpoint_index(records)
        return STEPS[progress] if progress < len(STEPS) else ()
    finally:
        os.close(descriptor)
