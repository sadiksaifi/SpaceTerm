#!/usr/bin/env python3
"""Keep release validation steps aligned with the shared macOS gate."""

import re
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    with (ROOT / ".mise.toml").open("rb") as source:
        tasks = tomllib.load(source)["tasks"]

    def leaves(name):
        command = tasks[name]["run"]
        if not isinstance(command, list) or not all(
            isinstance(entry, dict) and "task" in entry for entry in command
        ):
            return [name]
        result = []
        for entry in command:
            result.extend(leaves(entry["task"]))
        return result

    expected = leaves("validate:macos")
    workflow = (ROOT / ".github/workflows/release.yml").read_text()
    validation = workflow.split("\n  validate:\n", 1)[1].split("\n  package:\n", 1)[0]
    actual = re.findall(r"(?m)^        run: mise run ([\w:-]+)$", validation)
    assert actual == ["deps:metal:macos", *expected], (
        f"release validation steps differ from validate:macos:\nexpected {expected}\nactual {actual}"
    )
    publication = workflow.split("\n  publish:\n", 1)[1]
    assert "needs: [preflight, validate, package]" in publication, (
        "publishing must wait for both macOS gates"
    )
    # mise run installs every missing configured tool unless this is disabled,
    # which would undo a job's partial install_args.
    jobs = re.split(r"(?m)^  ([\w-]+):\n", workflow.split("\njobs:\n", 1)[1])[1:]
    for name, body in zip(jobs[::2], jobs[1::2]):
        if "install_args:" in body:
            assert 'MISE_TASK_RUN_AUTO_INSTALL: "false"' in body, (
                f"{name} installs selected tools, so mise run must not install the rest"
            )
    print("Release workflow gates publication on every macOS validation task and package.")


if __name__ == "__main__":
    main()
