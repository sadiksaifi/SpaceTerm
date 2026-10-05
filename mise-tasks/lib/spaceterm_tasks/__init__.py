"""Shared code for SpaceTerm's mise file tasks. Standard library only."""

import errno
import signal
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


class TaskError(Exception):
    """A content-free failure a task reports without a traceback."""


def main(entry):
    """Run a task entry point and report TaskError or OSError as a content-free line."""
    # Imported here because processes imports this package.
    from spaceterm_tasks.processes import terminating_signals_raise

    try:
        # Termination unwinds the task so its cleanup, such as detaching a disk image, runs.
        with terminating_signals_raise():
            status = entry()
    except KeyboardInterrupt:
        raise SystemExit(128 + signal.SIGINT) from None
    except TaskError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from None
    except OSError as error:
        # Native errors name paths; report only their classification.
        kind = errno.errorcode.get(error.errno or 0, type(error).__name__)
        print(f"error: an operating-system operation failed ({kind})", file=sys.stderr)
        raise SystemExit(1) from None
    raise SystemExit(status or 0)
