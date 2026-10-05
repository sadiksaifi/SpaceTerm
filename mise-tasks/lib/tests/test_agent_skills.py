"""Exercise the Claude Code link to the shared agent skills."""

import os
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.agent_skills import TARGET, link_claude_code


class ClaudeCodeLinkTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.skill = self.root / ".agents" / "skills" / "example" / "SKILL.md"
        self.skill.parent.mkdir(parents=True)
        self.skill.write_text("example")
        self.link = self.root / ".claude" / "skills"

    def test_link_reaches_the_shared_skills_and_is_idempotent(self):
        for _ in range(2):
            link_claude_code(self.root)
        self.assertEqual(Path(os.readlink(self.link)), TARGET)
        self.assertEqual((self.link / "example" / "SKILL.md").read_text(), "example")

    def test_existing_directory_or_foreign_link_is_preserved(self):
        self.link.mkdir(parents=True)
        (self.link / "own").write_text("own")
        with self.assertRaises(TaskError):
            link_claude_code(self.root)
        self.assertEqual((self.link / "own").read_text(), "own")

        (self.link / "own").unlink()
        self.link.rmdir()
        os.symlink(self.root, self.link, target_is_directory=True)
        with self.assertRaises(TaskError):
            link_claude_code(self.root)
        self.assertEqual(Path(os.readlink(self.link)), self.root)


if __name__ == "__main__":
    unittest.main()
