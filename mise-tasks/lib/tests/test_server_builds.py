"""Both local server builds must describe the same current working tree."""

import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks.server_builds import _source, prepare


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def project(root):
    root.mkdir()
    git(root, "init", "-q")
    git(root, "config", "user.name", "Fixture")
    git(root, "config", "user.email", "fixture@example.invalid")
    (root / "source.rs").write_text("original")
    (root / ".gitignore").write_text("target/\nprivate\n")
    git(root, "add", ".")
    git(root, "commit", "-qm", "initial")


class ServerSourceTests(unittest.TestCase):
    def test_fingerprint_tracks_current_source_and_ignores_private_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "project"
            project(root)
            original = _source(root)[2]
            (root / "private").write_text("ignored")
            self.assertEqual(_source(root)[2], original)
            (root / "source.rs").write_text("dirty")
            dirty = _source(root)[2]
            self.assertNotEqual(dirty, original)
            (root / "untracked.rs").write_text("current")
            untracked = _source(root)[2]
            self.assertNotEqual(untracked, dirty)
            (root / "source.rs").unlink()
            self.assertNotEqual(_source(root)[2], untracked)

    def test_changed_source_during_build_cannot_publish_a_catalog(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "project"
            project(root)
            destination = Path(temporary) / "catalog"

            def changed_build(identity, *, target):
                (root / "source.rs").write_text("changed during compilation")
                return root / "archive", root / "signature"

            with (
                patch("spaceterm_tasks.server_builds.ROOT", root),
                patch("spaceterm_tasks.server_builds.package", changed_build),
                self.assertRaisesRegex(TaskError, "source changed during server catalog"),
            ):
                prepare(destination, "spaceterm-preflight")
            self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main()
