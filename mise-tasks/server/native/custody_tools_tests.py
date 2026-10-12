#!/usr/bin/env python3
"""Run the unprivileged custody artifact and headless regressions, not native evidence."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))

from spaceterm_tasks import TaskError, main


def test():
    suite = unittest.defaultTestLoader.loadTestsFromNames(
        [
            "tests.test_headless",
            "tests.test_server_artifacts",
            "tests.test_package_server",
            "tests.test_linux_custody_packaging",
            "tests.test_linux_custody_evidence",
        ]
    )
    count = suite.countTestCases()
    if count == 0:
        raise TaskError("custody tooling regression gate has no tests")
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if not result.wasSuccessful() or result.skipped or result.testsRun != count:
        raise TaskError("custody tooling regression gate did not execute every test")
    print(f"Executed {count} custody tooling tests; privileged evidence remains unrun.")


if __name__ == "__main__":
    main(test)
