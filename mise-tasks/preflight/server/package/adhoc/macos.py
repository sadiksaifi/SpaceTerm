#!/usr/bin/env -S python -I
# MISE description="Create a disposable non-release macOS server archive with ad hoc signing"
# USAGE flag "--executable <executable>" help="Already built Development or Preflight server" required=#true
# USAGE flag "--output <output>" help="New test archive, never installation or release evidence" required=#true
"""No signing identity import, Keychain mutation or service installation."""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.packaged_headless_macos import prepare_cli

    main(lambda: prepare_cli(sys.argv[1:]))
