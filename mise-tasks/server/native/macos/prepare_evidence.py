#!/usr/bin/env -S python -I
# MISE description="Assemble existing signed native fixtures without signing imports or host changes"
"""Run only trusted repository task code, not modules beside supplied fixtures."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "lib"))

from spaceterm_tasks import main
from spaceterm_tasks.server_native_macos import prepare_task

if __name__ == "__main__":
    main(prepare_task)
