#!/usr/bin/env python
# MISE description="Test portable linux server package and release policies"
"""Run unprivileged package regressions, not native signing or baseline evidence."""

import argparse
import sys
import unittest

from spaceterm_tasks import TaskError, main


def test():
    if sys.platform != "linux":
        raise TaskError("linux server package tests require linux")
    suite = unittest.defaultTestLoader.loadTestsFromNames(
        [
            "tests.test_package_server",
            "tests.test_server_artifacts",
            "tests.test_release_server",
            "tests.test_macos_signing.SigningSerializationTests",
        ]
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if result.testsRun == 0 or result.skipped or not result.wasSuccessful():
        raise TaskError("server package regression gate did not execute every test")


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(test)
