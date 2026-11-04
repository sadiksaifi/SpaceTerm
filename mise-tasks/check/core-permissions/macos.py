#!/usr/bin/env python
# MISE description="Owner check after signing: run ssh to a LAN host and an Apple Event in a pane of the installed SpaceTerm Core"
# USAGE arg "<host>" help="A reachable LAN host that runs an SSH server"
# USAGE flag "--launcher <path>" help="The installed SpaceTerm Core launcher" default="~/.local/bin/spaceterm"
"""Run ssh to a LAN host and an Apple Event in a pane of the installed, signed SpaceTerm Core.

This may show macOS privacy prompts naming SpaceTerm Core. No automated test runs it.
"""

import argparse
from pathlib import Path

from spaceterm_tasks import main
from spaceterm_tasks.core_permissions import check

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--launcher", default="~/.local/bin/spaceterm")
    args = parser.parse_args()
    main(lambda: check(args.host, Path(args.launcher).expanduser()))
