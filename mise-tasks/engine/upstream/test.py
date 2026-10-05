#!/usr/bin/env python
# MISE description="Run upstream native VT tests in a prepared Ghostty source copy"
# USAGE arg "<source>" help="Prepared Ghostty source directory"
# USAGE flag "--filter <filter>" help="Run Zig tests whose names contain this text"
"""Run native VT tests from the prepared source that owns their relative fixtures."""

import argparse
from pathlib import Path

from spaceterm_tasks import main
from spaceterm_tasks.ghostty import run_native_tests


def test():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--filter")
    arguments = parser.parse_args()
    return run_native_tests(arguments.source, test_filter=arguments.filter)


if __name__ == "__main__":
    main(test)
