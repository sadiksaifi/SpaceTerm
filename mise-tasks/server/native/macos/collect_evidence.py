#!/usr/bin/env -S python -I
# MISE description="Validate bounded native evidence without repairing or filling missing records"
"""Collected files are data. Ignore PYTHONPATH and the supplied file's directory."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "lib"))

from spaceterm_tasks import main
from spaceterm_tasks.server_native_macos import collect_task

if __name__ == "__main__":
    main(collect_task)
