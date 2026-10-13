#!/usr/bin/env -S python -I
# MISE description="Resume the reviewed local owner sequence without repeating earlier steps"
"""Owner actions remain explicit. This task never invokes sudo, logout or reboot."""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks/lib"))
sys.path.insert(0, str(ROOT / "packaging/macos/native-evidence"))

from assembly import (  # noqa: E402  # ty: ignore[unresolved-import]
    DEFAULT_PREPARED,
    task_main,
    verify_candidate,
)
from sequence import (  # noqa: E402  # ty: ignore[unresolved-import]
    AUTHENTICATION,
    OUTCOMES,
    checkpoint,
    checkpoint_status,
    default_state,
    read_outcomes,
    read_preflight,
)
from spaceterm_tasks import TaskError  # noqa: E402
from spaceterm_tasks.server_native_macos import EvidenceArgumentParser  # noqa: E402


def entry():
    parser = EvidenceArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("local", "disposable"), default="local")
    parser.add_argument("--candidate", type=Path, default=DEFAULT_PREPARED)
    parser.add_argument("--state", type=Path)
    parser.add_argument("--step", type=int)
    parser.add_argument("--outcome", choices=OUTCOMES)
    parser.add_argument("--resolve", action="store_true")
    parser.add_argument("--gui-authentication", choices=AUTHENTICATION)
    parser.add_argument("--daemon-authentication", choices=AUTHENTICATION)
    arguments = parser.parse_args()
    if (
        arguments.profile != "local"
        or (arguments.step is None) != (arguments.outcome is None)
        or (arguments.resolve and arguments.step is None)
        or (arguments.gui_authentication is None) != (arguments.daemon_authentication is None)
        or (
            arguments.gui_authentication is not None and (arguments.step != 13 or arguments.resolve)
        )
    ):
        # Spare-Mac automation was removed by the owner. Jammy uses the owned VM
        # executor and its separate bounded sequence, never this Mac task.
        raise TaskError("invalid-record")
    identity = read_preflight(arguments.candidate)
    for name in ("baseline", "update"):
        verify_candidate(arguments.candidate / name)
    state = (arguments.state or default_state(identity["run_id"])).absolute()
    methods = (
        (arguments.gui_authentication, arguments.daemon_authentication)
        if arguments.gui_authentication is not None
        else None
    )
    next_step = checkpoint(
        state,
        identity,
        arguments.step,
        arguments.outcome,
        resolve=arguments.resolve,
        authentication=methods,
    )
    status = checkpoint_status(read_outcomes(state, identity))
    if next_step is None:
        print(f"phase=owner-checkpoint classification={status} state={state} native_passed=0")
        return 2
    if next_step:
        print(
            f"phase=owner-checkpoint classification={status} next_step={next_step[0]} state={state} native_passed=0"
        )
    else:
        print(f"phase=owner-checkpoint classification=notes-complete state={state} native_passed=0")
    return 0


if __name__ == "__main__":
    task_main(entry)
