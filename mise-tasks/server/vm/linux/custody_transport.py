#!/usr/bin/env -S python -I
"""Provision one owned stock Jammy guest, verify pinned SSH, then tear it down.

This checks provisioning and transport only. It does not execute or credit the
privileged custody suites. The coordinator runs it later from the Linux host,
using separately retained public input pins in a private local approval file.
"""

import sys

if not sys.flags.isolated:
    print("classification=isolated-python-required", file=sys.stderr)
    raise SystemExit(2)

from pathlib import Path  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "lib"))

from spaceterm_tasks import main  # noqa: E402
from spaceterm_tasks.server_vm import Phase, VmArgumentParser  # noqa: E402
from spaceterm_tasks.server_vm_linux import owned_vm_transport  # noqa: E402


def run():
    parser = VmArgumentParser(Phase.PREPARE, description=__doc__)
    parser.add_argument("--disposable-jammy", action="store_true", required=True)
    parser.add_argument("--approval", type=Path, required=True)
    arguments = parser.parse_args()
    owned_vm_transport(arguments.approval)
    print("classification=transport-passed custody-evidence=unrun")
    return 0


if __name__ == "__main__":
    main(run)
