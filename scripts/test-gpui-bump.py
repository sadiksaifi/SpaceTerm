#!/usr/bin/env python3
"""Exercise GPUI fork bumps in an isolated repository without network access."""

import importlib.util
from contextlib import redirect_stderr
import io
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("gpui-bump.py")
SPEC = importlib.util.spec_from_file_location("gpui_bump", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)

OLD_TAG = "spaceterm-2026-09-26"
NEW_TAG = "spaceterm-2026-10-26"
FORK_URL = "https://github.com/sadiksaifi/zed"
MANIFEST = f'''[package]
name = "fixture"
version = "0.1.0"

[dependencies]
gpui = {{ git = "{FORK_URL}", tag = "{OLD_TAG}" }}
gpui_platform = {{ git = "{FORK_URL}", tag = "{OLD_TAG}", features = ["font-kit"] }}
serde = "1.0"
other = {{ git = "https://example.com/other", tag = "{OLD_TAG}" }}

[dev-dependencies]
gpui = {{ git = "{FORK_URL}", tag = "{OLD_TAG}", features = ["test-support"] }}
gpui_macos = {{ git = "{FORK_URL}", tag = "{OLD_TAG}" }}
'''
LOCK_NAMES = ("gpui", "gpui_apple", "gpui_macos", "gpui_platform")


def lockfile(tag):
    packages = [
        f'''[[package]]
name = "{name}"
version = "0.2.0"
source = "git+{FORK_URL}?tag={tag}#1234"
'''
        for name in LOCK_NAMES
    ]
    packages.append('''[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
''')
    return "version = 4\n\n" + "\n".join(packages)


class StubRemote:
    def __init__(self, exists=True, toolchain='[toolchain]\nchannel = "1.99.0"\n'):
        self.exists = exists
        self.contents = toolchain
        self.queries = []

    def has_tag(self, tag):
        self.queries.append(("tag", tag))
        return self.exists

    def toolchain(self, tag):
        self.queries.append(("toolchain", tag))
        return self.contents


class GpuiBumpTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        (self.root / "Cargo.toml").write_text(MANIFEST)
        (self.root / "Cargo.lock").write_text(lockfile(OLD_TAG))
        (self.root / "rust-toolchain.toml").write_text(
            '[toolchain]\nchannel = "1.98.1"\nprofile = "minimal"\ncomponents = ["rustfmt"]\n'
        )
        self.remote = StubRemote()
        self.updates = []

    def update(self, root, packages):
        self.updates.append((root, packages))
        (root / "Cargo.lock").write_text(lockfile(NEW_TAG))

    def bump(self):
        return MODULE.bump(self.root, NEW_TAG, self.remote, self.update)

    def snapshot(self):
        return tuple((self.root / name).read_bytes() for name in (
            "Cargo.toml", "Cargo.lock", "rust-toolchain.toml"
        ))

    def assert_fails_without_changes(self, kind):
        before = self.snapshot()
        with self.assertRaises(MODULE.BumpError) as raised:
            self.bump()
        self.assertEqual(raised.exception.kind, kind)
        self.assertEqual(self.snapshot(), before)
        self.assertEqual(self.updates, [])

    def test_bump_updates_only_fork_fields_and_packages(self):
        result = self.bump()
        self.assertEqual(result, MODULE.BumpResult(OLD_TAG, NEW_TAG, "1.98.1", "1.99.0"))
        expected = MANIFEST.replace(
            f'git = "{FORK_URL}", tag = "{OLD_TAG}"',
            f'git = "{FORK_URL}", tag = "{NEW_TAG}"',
        )
        self.assertEqual((self.root / "Cargo.toml").read_text(), expected)
        self.assertEqual((self.root / "Cargo.lock").read_text(), lockfile(NEW_TAG))
        self.assertEqual((self.root / "rust-toolchain.toml").read_text(),
            '[toolchain]\nchannel = "1.99.0"\nprofile = "minimal"\ncomponents = ["rustfmt"]\n')
        self.assertEqual(self.updates, [(self.root, list(LOCK_NAMES))])
        self.assertEqual(self.remote.queries, [("tag", NEW_TAG), ("toolchain", NEW_TAG)])

    def test_missing_tag_preserves_tree(self):
        self.remote.exists = False
        self.assert_fails_without_changes(MODULE.Failure.TAG_MISSING)
        self.assertEqual(self.remote.queries, [("tag", NEW_TAG)])

    def test_local_override_preserves_tree(self):
        config = self.root / ".cargo" / "config.toml"
        config.parent.mkdir()
        config.write_text(f'[patch."{FORK_URL}"]\ngpui = {{ path = "/tmp/zed/gpui" }}\n')
        self.assert_fails_without_changes(MODULE.Failure.OVERRIDE_ACTIVE)
        self.assertEqual(self.remote.queries, [])

    def test_unexpected_dependency_set_preserves_tree(self):
        manifest = self.root / "Cargo.toml"
        manifest.write_text(MANIFEST.replace("gpui_platform =", "different ="))
        self.assert_fails_without_changes(MODULE.Failure.DEPENDENCIES_UNEXPECTED)
        self.assertEqual(self.remote.queries, [])

    def test_remote_toolchain_without_channel_preserves_tree(self):
        self.remote.contents = '[toolchain]\nprofile = "minimal"\n'
        self.assert_fails_without_changes(MODULE.Failure.TOOLCHAIN_INVALID)

    def test_repeated_bump_is_idempotent(self):
        self.bump()
        after_first = self.snapshot()
        result = self.bump()
        self.assertEqual(result, MODULE.BumpResult(NEW_TAG, NEW_TAG, "1.99.0", "1.99.0"))
        self.assertEqual(self.snapshot(), after_first)
        self.assertEqual(len(self.updates), 1)

    def test_failed_update_restores_all_originals(self):
        original = self.snapshot()

        def fail_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise MODULE.BumpError(MODULE.Failure.UPDATE_FAILED)

        self.update = fail_update
        with self.assertRaises(MODULE.BumpError) as raised:
            self.bump()
        self.assertEqual(raised.exception.kind, MODULE.Failure.UPDATE_FAILED)
        self.assertEqual(self.snapshot(), original)

    def test_retry_after_failed_update_succeeds(self):
        original = self.snapshot()
        successful_update = self.update

        def fail_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise MODULE.BumpError(MODULE.Failure.UPDATE_FAILED)

        self.update = fail_update
        with self.assertRaises(MODULE.BumpError):
            self.bump()
        self.assertEqual(self.snapshot(), original)
        self.update = successful_update
        result = self.bump()
        self.assertEqual(result.new_tag, NEW_TAG)
        self.assertEqual((self.root / "Cargo.lock").read_text(), lockfile(NEW_TAG))

    def test_failed_toolchain_write_restores_all_originals(self):
        original = self.snapshot()
        toolchain = self.root / "rust-toolchain.toml"
        original_replace = Path.replace
        failed = False

        def fail_once(source, target):
            nonlocal failed
            if target == toolchain and not failed:
                failed = True
                raise OSError("injected toolchain write failure")
            return original_replace(source, target)

        with patch.object(Path, "replace", fail_once):
            with self.assertRaises(MODULE.BumpError) as raised:
                self.bump()
        self.assertEqual(raised.exception.kind, MODULE.Failure.FILE_WRITE_FAILED)
        self.assertTrue(failed)
        self.assertEqual(self.snapshot(), original)
        self.assertEqual(self.updates, [])
        self.assertEqual(list(self.root.glob(".rust-toolchain.toml.*")), [])

    def test_interrupt_during_update_restores_all_originals(self):
        original = self.snapshot()

        def interrupt_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise KeyboardInterrupt

        self.update = interrupt_update
        with self.assertRaises(KeyboardInterrupt):
            self.bump()
        self.assertEqual(self.snapshot(), original)

    @unittest.skipUnless(os.name == "posix", "requires POSIX signals")
    def test_sigterm_during_update_restores_all_originals(self):
        original = self.snapshot()

        def interrupt_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            os.kill(os.getpid(), signal.SIGTERM)

        self.update = interrupt_update
        with self.assertRaises(MODULE.BumpCancelled):
            self.bump()
        self.assertEqual(self.snapshot(), original)

    def test_system_exit_during_update_restores_all_originals(self):
        original = self.snapshot()

        def interrupt_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise SystemExit(2)

        self.update = interrupt_update
        with self.assertRaises(SystemExit):
            self.bump()
        self.assertEqual(self.snapshot(), original)

    @unittest.skipUnless(os.name == "posix", "requires POSIX signals")
    def test_interrupt_during_restoration_finishes_restoration(self):
        original = self.snapshot()

        def fail_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise MODULE.BumpError(MODULE.Failure.UPDATE_FAILED)

        self.update = fail_update
        original_write = MODULE.atomic_write
        interrupted = False

        def interrupt_restore(path, contents, mode):
            nonlocal interrupted
            if path == self.root / "Cargo.toml" and contents == original[0] and not interrupted:
                interrupted = True
                os.kill(os.getpid(), signal.SIGINT)
            return original_write(path, contents, mode)

        with patch.object(MODULE, "atomic_write", interrupt_restore):
            with self.assertRaises(MODULE.BumpCancelled):
                self.bump()
        self.assertTrue(interrupted)
        self.assertEqual(self.snapshot(), original)

    def test_raised_interrupt_during_restoration_retries_the_file(self):
        original = self.snapshot()

        def fail_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise MODULE.BumpError(MODULE.Failure.UPDATE_FAILED)

        self.update = fail_update
        original_write = MODULE.atomic_write
        interrupted = False

        def interrupt_restore(path, contents, mode):
            nonlocal interrupted
            if path == self.root / "Cargo.toml" and contents == original[0] and not interrupted:
                interrupted = True
                raise KeyboardInterrupt
            return original_write(path, contents, mode)

        with patch.object(MODULE, "atomic_write", interrupt_restore):
            with self.assertRaises(MODULE.BumpCancelled):
                self.bump()
        self.assertTrue(interrupted)
        self.assertEqual(self.snapshot(), original)

    def test_restore_failure_still_attempts_other_files(self):
        original = self.snapshot()

        def fail_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise MODULE.BumpError(MODULE.Failure.UPDATE_FAILED)

        self.update = fail_update
        original_write = MODULE.atomic_write
        attempted = []

        def fail_manifest_restore(path, contents, mode):
            if contents in original:
                attempted.append(path.name)
            if path == self.root / "Cargo.toml" and contents == original[0]:
                raise OSError("injected restore failure")
            return original_write(path, contents, mode)

        with patch.object(MODULE, "atomic_write", fail_manifest_restore):
            with self.assertRaises(MODULE.BumpError) as raised:
                self.bump()
        self.assertEqual(raised.exception.kind, MODULE.Failure.RESTORE_FAILED)
        self.assertEqual(attempted, ["Cargo.toml", "rust-toolchain.toml", "Cargo.lock"])
        self.assertNotEqual(self.snapshot()[0], original[0])
        self.assertEqual(self.snapshot()[1:], original[1:])

    def test_cli_reports_cancellation_without_traceback(self):
        for interruption in (KeyboardInterrupt, SystemExit(2), MODULE.BumpCancelled):
            with self.subTest(interruption=interruption):
                output = io.StringIO()
                with patch.object(sys, "argv", ["gpui-bump.py", NEW_TAG]):
                    with patch.object(MODULE, "bump", side_effect=interruption):
                        with redirect_stderr(output), self.assertRaises(SystemExit) as raised:
                            MODULE.main()
                self.assertEqual(raised.exception.code, 130)
                self.assertEqual(output.getvalue(), "gpui:bump: cancelled\n")

    def test_cli_reports_failure_without_traceback(self):
        for failure, message in (
            (MODULE.BumpError(MODULE.Failure.UPDATE_FAILED),
             "gpui:bump: cargo update failed for the fork packages\n"),
            (RuntimeError("sensitive details"), "gpui:bump: failed\n"),
        ):
            with self.subTest(failure=failure):
                output = io.StringIO()
                with patch.object(sys, "argv", ["gpui-bump.py", NEW_TAG]):
                    with patch.object(MODULE, "bump", side_effect=failure):
                        with redirect_stderr(output), self.assertRaises(SystemExit) as raised:
                            MODULE.main()
                self.assertNotEqual(raised.exception.code, 0)
                self.assertEqual(output.getvalue(), message)

    def test_cargo_update_scopes_the_command_to_lockfile_packages(self):
        with patch.object(MODULE.subprocess, "Popen") as popen:
            popen.return_value.wait.return_value = 0
            MODULE.cargo_update(self.root, list(LOCK_NAMES))
        command = ["cargo", "update"]
        for name in LOCK_NAMES:
            command.extend(("--package", name))
        popen.assert_called_once_with(
            command,
            cwd=self.root,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=os.name == "posix",
        )

    def test_cargo_child_is_stopped_after_interruption(self):
        with patch.object(MODULE.subprocess, "Popen") as popen:
            process = popen.return_value
            process.pid = 12345
            process.poll.return_value = None
            process.wait.side_effect = [KeyboardInterrupt, 0]
            with patch.object(MODULE.os, "killpg", create=True) as killpg:
                with self.assertRaises(KeyboardInterrupt):
                    MODULE.cargo_update(self.root, list(LOCK_NAMES))
        if os.name == "posix":
            killpg.assert_called_once_with(12345, signal.SIGTERM)
        else:
            process.terminate.assert_called_once_with()
        self.assertEqual(process.wait.call_count, 2)

    @unittest.skipUnless(os.name == "posix", "requires POSIX signals")
    def test_cargo_child_is_stopped_when_interrupted_during_spawn(self):
        with patch.object(MODULE.subprocess, "Popen") as popen:
            process = popen.return_value
            process.pid = 12345
            process.poll.return_value = None
            process.wait.return_value = 0

            def spawn(*args, **kwargs):
                os.kill(os.getpid(), signal.SIGTERM)
                return process

            popen.side_effect = spawn
            with patch.object(MODULE.os, "killpg", create=True) as killpg:
                with self.assertRaises(MODULE.BumpCancelled):
                    MODULE.cargo_update(self.root, list(LOCK_NAMES))
        if os.name == "posix":
            killpg.assert_called_once_with(12345, signal.SIGTERM)
        else:
            process.terminate.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
