"""Native builders receive the current source without changing an existing checkout."""

import subprocess
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.server_builds import snapshot


class ServerSourceTests(unittest.TestCase):
    def test_snapshot_retains_dirty_deleted_untracked_and_submodule_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            root = parent / "project"
            root.mkdir()

            def git(directory, *arguments):
                return subprocess.run(
                    ["git", "-C", str(directory), *arguments],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip()

            for directory in (root, root / "dependency"):
                directory.mkdir(exist_ok=True)
                git(directory, "init", "-q")
                git(directory, "config", "user.name", "Fixture")
                git(directory, "config", "user.email", "fixture@example.invalid")
                (directory / "source.rs").write_text("original")
                git(directory, "add", ".")
                git(directory, "commit", "-qm", "initial")
            (root / ".gitmodules").write_text(
                '[submodule "dependency"]\npath = dependency\nurl = ./dependency\n'
            )
            (root / "deleted.rs").write_text("removed")
            (root / ".gitignore").write_text("target/\nprivate\n")
            git(root, "add", ".")
            git(root, "commit", "-qm", "dependency")
            commit = git(root, "rev-parse", "HEAD")
            (root / "source.rs").write_text("dirty main")
            (root / "dependency/source.rs").write_text("dirty dependency")
            (root / "new.rs").write_text("untracked")
            (root / "deleted.rs").unlink()
            (root / "private").write_text("ignored")
            git(root, "remote", "add", "private", "ssh://private-builder.invalid/source")
            before = git(root, "status", "--porcelain")
            first, digest = snapshot(root, parent / "snapshots")
            self.assertEqual((first / "source.rs").read_text(), "dirty main")
            self.assertEqual((first / "dependency/source.rs").read_text(), "dirty dependency")
            self.assertEqual((first / "new.rs").read_text(), "untracked")
            self.assertFalse((first / "deleted.rs").exists())
            self.assertFalse((first / "private").exists())
            self.assertEqual(git(first, "rev-parse", "HEAD"), commit)
            self.assertNotIn("private-builder", (first / ".git/config").read_text())
            self.assertFalse((first / ".git/hooks").exists())
            self.assertEqual(git(root, "status", "--porcelain"), before)
            self.assertEqual(snapshot(root, parent / "snapshots"), (first, digest))
            (root / "source.rs").write_text("new dirty source on same commit")
            second, changed = snapshot(root, parent / "snapshots")
            self.assertNotEqual(changed, digest)
            self.assertEqual((first / "source.rs").read_text(), "dirty main")
            self.assertEqual((second / "source.rs").read_text(), "new dirty source on same commit")
            for selected in (str(root / "dependency"), "../personal-checkout"):
                (root / ".cargo").mkdir(exist_ok=True)
                (root / ".cargo/config.toml").write_text(
                    "[patch.crates-io]\nprivate = { path = "
                    + repr(selected).replace("'", '"')
                    + " }\n"
                )
                with self.assertRaisesRegex(TaskError, "project-relative Cargo patches"):
                    snapshot(root, parent / "snapshots")


if __name__ == "__main__":
    unittest.main()
