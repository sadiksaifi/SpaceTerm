#!/usr/bin/env python
# MISE description="Inspect native server feasibility without changing host integration"
"""Read-only feasibility entry point; no native inspector is configured."""

from spaceterm_tasks import main
from spaceterm_tasks.server_native import Phase, run_task

if __name__ == "__main__":
    main(lambda: run_task(Phase.INSPECT))
