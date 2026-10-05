"""Shared code for SpaceTerm's mise file tasks. Standard library only."""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


class TaskError(Exception):
    """A content-free failure a task reports without a traceback."""


def main(entry):
    """Run a task entry point and report TaskError as a one-line failure."""
    try:
        status = entry()
    except TaskError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from None
    raise SystemExit(status or 0)
