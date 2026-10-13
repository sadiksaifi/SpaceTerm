#!/usr/bin/env -S python -I
# MISE description="Test native evidence composition with injected native observations and storage"
"""Exercise the real host interfaces and collector, without privileged effects.

The Rust integration tests supply native peers only in test composition. Their
JSONL is discarded after schema validation and does not credit host evidence.
No production executable, signing operation, Keychain or service manager runs.
"""

import io
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402
from spaceterm_tasks.native_evidence_schema import (  # noqa: E402
    RequiredCase,
    collect,
    decode_json,
    validate_record,
)
from spaceterm_tasks.server_native_macos import required_cases  # noqa: E402


def run() -> None:
    with tempfile.TemporaryDirectory(prefix="spaceterm-evidence-composition-") as directory:
        output = Path(directory) / "observations.jsonl"
        environment = signing_environment(os.environ)
        environment["SPACETERM_TEST_EVIDENCE_OUTPUT"] = str(output)
        if sys.platform == "darwin":
            environment["CARGO_BUILD_JOBS"] = "3"
        command = [
            "cargo",
            "test",
            "--package",
            "spaceterm-host",
            "--features",
            "native-tests",
            "--locked",
            "--test",
            "native_evidence",
            "--test",
            "native_receipts",
        ]
        try:
            result = subprocess.run(
                command, cwd=ROOT, env=environment, capture_output=True, text=True, timeout=240
            )
        except (OSError, subprocess.TimeoutExpired):
            raise TaskError("native-unavailable") from None
        # Never relay a tool's text, paths or inherited values on failure.
        if result.returncode:
            raise TaskError("invalid-record")
        counts = re.findall(
            r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", result.stdout
        )
        if len(counts) != 2 or any(
            int(passed) == 0 or int(failed) or int(ignored) for passed, failed, ignored in counts
        ):
            raise TaskError("invalid-record")
        requirements = required_cases("local")
        with output.open("rb") as source:
            summary = collect(source, requirements)
        # Assert independent interface expectations, not merely two successful
        # binaries. Both contexts retain the baseline, not private peer equality.
        records = [decode_json(line) for line in output.read_bytes().splitlines()]
        comparisons = [value for value in records if value["case"] == "normalized-shell-comparison"]
        if (
            {value["context"] for value in comparisons} != {"gui-parented", "daemon-parented"}
            or any(value["facts"]["reference"] != "sanitized-launch-v1" for value in comparisons)
            or summary.records != len(requirements) + len(comparisons)
            or summary.unrun
            or summary.unresolved
        ):
            raise TaskError("invalid-record")
        for context in ("gui-parented", "daemon-parented"):
            missing = [
                value
                for value in records
                if (value["case"], value["context"]) != ("normalized-shell-comparison", context)
            ]
            payload = b"".join(
                json.dumps(dict(value, sequence=sequence)).encode() + b"\n"
                for sequence, value in enumerate(missing, 1)
            )
            try:
                collect(io.BytesIO(payload), requirements)
            except TaskError as error:
                if str(error) != "invalid-record":
                    raise
            else:
                raise TaskError("invalid-record")
        # The Rust producer keeps a failed root-group observation. Changing only
        # its claimed result must never make Python credit the privilege drop.
        root_group = decode_json(output.with_suffix(".root-group.jsonl").read_bytes())
        validate_record(root_group)
        if root_group["result"] != "failed" or root_group["facts"]["supplementary_groups"] != [
            20,
            0,
        ]:
            raise TaskError("invalid-record")
        root_group.update(result="passed", failure=None)
        try:
            collect(
                io.BytesIO(json.dumps(root_group).encode() + b"\n"),
                [
                    RequiredCase(
                        "macos-launcher", "privilege-and-inheritance-proof", "daemon-parented"
                    )
                ],
            )
        except TaskError as error:
            if str(error) != "invalid-record":
                raise
        else:
            raise TaskError("invalid-record")
        tests = sum(int(passed) for passed, _, _ in counts)
        print(
            f"classification=composition-valid rust_tests={tests} records={summary.records} "
            "proof=injected native_credit=unrun"
        )


if __name__ == "__main__":
    main(run)
