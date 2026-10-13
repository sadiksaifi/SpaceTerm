#!/usr/bin/env -S python -I
"""Run legacy artifact inspectors in a closed process environment."""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "mise-tasks/lib"))
sys.path.insert(0, str(Path(__file__).parent))

from assembly import read_candidate, task_main  # noqa: E402
from spaceterm_tasks import TaskError, package_server, package_server_macos  # noqa: E402


def entry():
    if len(sys.argv) != 2 or not Path(sys.argv[1]).is_absolute():
        raise TaskError("invalid-record")
    root = Path(sys.argv[1])
    document = read_candidate(root)
    code = {
        root / entry["executable"]
        for entry in document["code"]
        if entry["name"].startswith("server/")
    }
    package_server_macos.verify_macho_closure(root / "server", code)
    package_server.verify_executable_metadata(root / "server", document["metadata"])
    return 0


if __name__ == "__main__":
    task_main(entry)
