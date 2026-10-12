#!/usr/bin/env -S python -I
"""Unresolved Linux composition regression, not privileged evidence or suite credit.

The injected peer cannot repair a missing shared native observation contract.
Keep this regression separate from mechanics gates until the schema-owning track
supplies the genuine Linux effect contract and the protected fixture export.
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "mise-tasks/lib"))

from spaceterm_tasks import TaskError
from spaceterm_tasks import native_evidence_schema as schema  # noqa: E402

from tests import test_linux_custody_evidence as fixtures  # noqa: E402


class LinuxCompositionTests(unittest.TestCase):
    def test_packaged_jammy_custody_requires_a_decisive_native_effect_contract(self):
        fixture = fixtures.CustodyEvidenceTests()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        required = (
            schema.RequiredCase(
                "server-distribution", "jammy-package-execution", "disposable-jammy"
            ),
        )
        # Refuse inside the native port, without fabricating a Passed record.
        # The current missing contract refuses before it can reach this port.
        fixture.peer.error = TaskError("native-unavailable")
        with self.assertRaisesRegex(TaskError, "^native-unavailable$"):
            fixture.driver.collect_probe(fixture.receipt, "packaged-execution", required)
        self.assertIn("run-packaged-execution", fixture.peer.calls)


if __name__ == "__main__":
    unittest.main()
