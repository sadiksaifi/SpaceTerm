"""Native builders receive the current source without changing an existing checkout."""

import signal
import subprocess
import sys
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks.server_builds import _sync_source, snapshot


class ServerSourceTests(unittest.TestCase):
    def test_concurrent_first_snapshots_reuse_the_verified_winner(self):
        self.concurrent_snapshots(corrupt_winner=False)

    def test_concurrent_snapshot_refuses_a_changed_publication_winner(self):
        self.concurrent_snapshots(corrupt_winner=True)

    def concurrent_snapshots(self, *, corrupt_winner):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            root = parent / "project"
            root.mkdir()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "source.rs").write_text("current dirty source")
            subprocess.run(["git", "-C", str(root), "add", "."], check=True)
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-qm",
                    "initial",
                ],
                check=True,
            )
            barrier = threading.Barrier(2)
            winner_ready = threading.Event()
            rename = Path.rename

            def simultaneous_publication(source, destination):
                # Control scheduling only. Both publications still use the real filesystem.
                barrier.wait(timeout=10)
                try:
                    published = rename(source, destination)
                except OSError:
                    self.assertTrue(winner_ready.wait(timeout=10))
                    raise
                if corrupt_winner:
                    (destination / "source.rs").write_text("changed after publication")
                winner_ready.set()
                return published

            with (
                patch.object(Path, "rename", simultaneous_publication),
                ThreadPoolExecutor(2) as workers,
            ):
                first = workers.submit(snapshot, root, parent / "snapshots")
                second = workers.submit(snapshot, root, parent / "snapshots")
                results, errors = [], []
                for worker in (first, second):
                    try:
                        results.append(worker.result(timeout=15))
                    except TaskError as error:
                        errors.append(str(error))
            if corrupt_winner:
                self.assertEqual(len(results), 1)
                self.assertEqual(len(errors), 1)
                self.assertIn("cached native builder source changed", errors[0])
            else:
                self.assertFalse(errors)
                self.assertEqual(results[0], results[1])
                published, _ = results[0]
                self.assertEqual((published / "source.rs").read_text(), "current dirty source")
            self.assertEqual(len(list((parent / "snapshots").iterdir())), 1)

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
            # Imported Python and native tools may leave ignored output in a snapshot.
            # The builder transport still sends only the listed source and Git identity.
            (first / "private").write_text("ignored credential fixture")
            transported = parent / "transported"
            transported.mkdir()
            _sync_source(first, str(transported) + "/", (), "source transfer failed")
            self.assertFalse((transported / "private").exists())
            self.assertEqual((transported / "new.rs").read_text(), "untracked")
            self.assertEqual(git(transported, "rev-parse", "HEAD"), commit)

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


class RemoteBuildLeaseTests(unittest.TestCase):
    def test_transport_keeps_the_remote_native_build_lease_alive(self):
        from spaceterm_tasks.server_builds import _remote_run

        peer = (
            "import select, sys; "
            "ready = select.select([sys.stdin], [], [], 2)[0]; "
            "raise SystemExit(0 if ready and sys.stdin.buffer.read(1) == b'.' else 23)"
        )
        _remote_run([sys.executable, "-c", peer], "the remote native build lost its lease")

    def test_native_runner_ends_on_eof_or_expired_transport_lease(self):
        for expired in (False, True):
            program = "from spaceterm_tasks.server_builds import _watch_stdin; "
            if expired:
                # Time is a boundary: expire select immediately instead of waiting 15 seconds.
                program += "import select; select.select = lambda *a: ([], [], []); "
            result = subprocess.run(
                [sys.executable, "-c", program + "_watch_stdin()"],
                stdin=subprocess.DEVNULL,
                capture_output=True,
                timeout=3,
            )
            self.assertEqual(result.returncode, -signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
