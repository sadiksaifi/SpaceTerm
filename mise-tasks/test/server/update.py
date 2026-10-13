#!/usr/bin/env -S python -I
# MISE description="Test signed server selection and real Git/PTY retirement without installing a service"
"""Run independent update regression evidence on macOS or Linux.

Signed archives use test signing keys. Service-manager effects use injected
providers. Git writes, PTYs and authenticated replacement connections are real.
This gate does not install a service, change linger, read production recovery
contents, import a certificate, or establish signed installed-service evidence.
Run it again against the coordinator's frozen candidate on both hosts.
"""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.account_service_checks import run

    main(lambda: run(["update", *sys.argv[1:]]))
