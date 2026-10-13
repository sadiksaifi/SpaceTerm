#!/usr/bin/env -S python -I
# MISE description="Run signed macOS server archive queries outside the checkout without GUI access"
# USAGE flag "--archive <archive>" help="Explicit macOS server archive" required=#true
# USAGE flag "--source-commit <source_commit>" help="Full source revision built into the candidate" required=#true
# USAGE flag "--allow-ad-hoc" help="Allow non-release ad hoc test signatures, not installation trust"
"""No service installation, Keychain mutation, release signing or lifecycle operation."""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.packaged_headless_macos import cli

    main(lambda: cli(sys.argv[1:]))
