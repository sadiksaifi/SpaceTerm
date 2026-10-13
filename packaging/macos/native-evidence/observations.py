"""Retain unbound wire input as synthetic observations, never native proof."""

import os
import platform
import stat
import struct

from spaceterm_tasks import TaskError
from spaceterm_tasks.native_evidence_schema import CAPABILITIES, encode_record, validate_record
from spaceterm_tasks.server_native_macos import file_snapshot


def read_input(path, maximum):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > maximum:
            raise TaskError("invalid-record")
        data = source.read(maximum + 1)
        if len(data) > maximum or file_snapshot(os.fstat(descriptor)) != file_snapshot(before):
            raise TaskError("invalid-record")
    return data


def version(text):
    parts = text.split(".")
    if not 1 <= len(parts) <= 3 or not all(
        part.isascii() and part.isdecimal() and len(part) <= 5 and int(part) <= 65535
        for part in parts
    ):
        raise TaskError("unsupported-host")
    return [int(part) for part in parts] + [0] * (3 - len(parts))


def native_host():
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise TaskError("unsupported-host")
    return {
        "platform": "macos",
        "architecture": "arm64",
        "os_version": version(platform.mac_ver()[0]),
        "kernel_version": version(platform.release()),
        "libc_version": None,
        "supervisor": "launchd",
        "supervisor_version": None,
    }


def wire_record(data, identity, host, sequence=1):
    failure = None
    if len(data) == 20:
        schema, context, reference, path, locale = struct.unpack(">5I", data)
        if schema != 2 or reference != 1 or path > 3 or locale > 2:
            raise TaskError("invalid-record")
        case = "normalized-shell-comparison"
        # Mode binding proves an actual login/interactive parent. The local wire
        # does not prove shell-integration startup order. Keep that observation
        # pending instead of crediting an incomplete case-13 comparison.
        result, failure = "pending", "outcome-pending"
        facts = {
            "kind": "environment-comparison",
            "reference": "sanitized-launch-v1",
            "path_difference": ("equal", "presence-changed", "components-changed", "order-changed")[
                path
            ],
            "locale_difference": ("equal", "presence-changed", "values-changed")[locale],
            "login": True,
            "interactive": True,
            "startup_order_verified": False,
        }
    elif len(data) == 32:
        schema, capability, context, outcome, responsible, preflight, effect, cleanup = (
            struct.unpack(">8I", data)
        )
        if (
            schema != 1
            or not 1 <= capability <= 13
            or not 1 <= outcome <= 10
            or not 1 <= responsible <= 6
            or preflight > 4
            or effect > 5
            or cleanup > 2
        ):
            raise TaskError("invalid-record")
        case = CAPABILITIES[capability - 1]
        result = (
            "passed",
            "denied",
            "cancelled",
            "unavailable",
            "timed-out",
            "unrun",
            "failed",
            "pending",
            "indeterminate",
            "cleanup-unresolved",
        )[outcome - 1]
        failure = {
            "denied": "authorization-denied",
            "cancelled": "authorization-cancelled",
            "unavailable": "native-unavailable",
            "timed-out": "timed-out",
            "unrun": "native-unavailable",
            "failed": "unsafe-native-state",
            "pending": "outcome-pending",
            "indeterminate": "indeterminate",
            "cleanup-unresolved": "cleanup-unresolved",
        }.get(result)
        facts = {
            "kind": "capability",
            "capability": capability,
            "preflight": ("unrun", "passed", "denied", "unrun", "unavailable")[preflight],
            "effect": ("unrun", "passed", "denied", "unavailable", "failed", "indeterminate")[
                effect
            ],
            "responsible_process": ("desktop", "server", "probe", "companion", "other", "unknown")[
                responsible - 1
            ],
            "cleanup": ("not-needed", "complete", "unresolved")[cleanup],
        }
    else:
        raise TaskError("invalid-record")
    if context not in (1, 2):
        raise TaskError("invalid-record")
    # This standalone file has no authenticated producer, run or artifact binding.
    # Even verified candidate bytes cannot prove who produced it or when.
    result, failure = "pending", "execution-binding-unproven"
    record = {
        "schema": 1,
        "scope": "native-deployment",
        "run_id": identity["run_id"],
        "sequence": sequence,
        "job": "macos-capabilities",
        "case": case,
        "host": host,
        "revision": identity["revision"],
        "dirty": identity["dirty"],
        "artifact_sha256": identity["artifact_sha256"],
        "context": ("gui-parented", "daemon-parented")[context - 1],
        "proof": "synthetic",
        "result": result,
        "failure": failure,
        "facts": facts,
    }
    validate_record(record)
    return record


def write_wire_record(path, data, identity, host):
    record = wire_record(data, identity, host)
    encoded = encode_record(record)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        if output.write(encoded) != len(encoded):
            raise TaskError("io-unavailable")
        output.flush()
        os.fsync(descriptor)
    return record
