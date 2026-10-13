#!/usr/bin/env python
# MISE description="Test macos server packaging, ad hoc archive execution and release policies"
"""Run package regressions and disposable ad hoc signing without service mutation."""

import argparse
import sys
import unittest

from spaceterm_tasks import TaskError, main


def test():
    if sys.platform != "darwin":
        raise TaskError("macos server package tests require macos")
    suite = unittest.defaultTestLoader.loadTestsFromNames(
        [
            "tests.test_package_server",
            "tests.test_server_artifacts",
            "tests.test_release_server",
            "tests.test_macos_signing.SigningSerializationTests",
            "tests.test_packaged_headless_macos",
        ]
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if result.testsRun == 0 or result.skipped or not result.wasSuccessful():
        raise TaskError("server package regression gate did not execute every test")


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(test)
