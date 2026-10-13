#!/usr/bin/env -S python -I
# MISE description="Run archived server queries without display, GUI libraries, client or checkout"
# USAGE flag "--archive <archive>" help="Explicit Linux server archive" required=#true
# USAGE flag "--source-commit <source_commit>" help="Full revision copied into the build checkout" required=#true
# USAGE flag "--require-baseline-host" help="Require actual glibc 2.35, kernel 5.15 and systemd 249"
"""Run unprivileged packaged queries. This does not install or exercise a service."""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.packaged_headless_linux import cli

    main(lambda: cli(sys.argv[1:]))
