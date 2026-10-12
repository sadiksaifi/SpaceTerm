#!/usr/bin/env -S python -I
# MISE description="Inspect native server feasibility without changing host integration"
"""Read registration candidates without Keychain access, prompts, or repair."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))

from spaceterm_tasks import main
from spaceterm_tasks.server_native import Phase, run_task

if __name__ == "__main__":
    main(lambda: run_task(Phase.INSPECT))
