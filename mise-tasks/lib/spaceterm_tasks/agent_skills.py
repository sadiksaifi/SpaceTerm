"""Share the installed agent skills with Claude Code.

`skills experimental_install` restores skills into `.agents/skills`. Claude Code reads
`.claude/skills`, so that path links to the shared directory.
"""

import os
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

TARGET = Path("..") / ".agents" / "skills"


def link_claude_code(root=ROOT):
    """Point `.claude/skills` at `.agents/skills`; never replace anything else at that path."""
    link = root / ".claude" / "skills"
    if link.is_symlink():
        if Path(os.readlink(link)) == TARGET:
            return
        raise TaskError(".claude/skills links elsewhere; remove it to share the agent skills")
    if link.exists():
        raise TaskError(".claude/skills already exists; remove it to share the agent skills")
    link.parent.mkdir(exist_ok=True)
    try:
        os.symlink(TARGET, link, target_is_directory=True)
    except OSError:
        # Windows creates symbolic links only with Developer Mode or elevation.
        raise TaskError("could not link .claude/skills; allow symbolic links and retry") from None
