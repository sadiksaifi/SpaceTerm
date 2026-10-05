#!/usr/bin/env python
# MISE description="Install the agent skills listed in skills-lock.json and share them with Claude Code"
# MISE env={DISABLE_TELEMETRY="1"}
"""Install the agent skills listed in skills-lock.json and share them with Claude Code."""

import argparse
import subprocess

from spaceterm_tasks import ROOT, TaskError, main
from spaceterm_tasks.agent_skills import link_claude_code


def setup():
    if subprocess.run(["skills", "experimental_install"], cwd=ROOT).returncode:
        raise TaskError("the agent skills could not be installed")
    link_claude_code()


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(setup)
