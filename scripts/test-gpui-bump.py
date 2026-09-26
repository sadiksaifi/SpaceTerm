#!/usr/bin/env python3
"""Exercise GPUI fork bumps in an isolated Git repository without network access."""

from contextlib import redirect_stderr
import importlib.util
import io
import json
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
TOOLCHAIN = '[toolchain]\nchannel = "1.98.1"\nprofile = "minimal"\ncomponents = ["rustfmt"]\n'
MEMBER = "crates/fixture-ui/Cargo.toml"
OTHER_MEMBER = "crates/fixture-core/Cargo.toml"
MEMBER_MANIFEST = f'''[package]
name = "fixture-ui"
version = "0.1.0"

[dependencies]
gpui = {{ git = "{FORK_URL}", tag = "{OLD_TAG}" }}

[dev-dependencies]
gpui = {{ git = "{FORK_URL}", tag = "{OLD_TAG}", features = ["test-support"] }}
'''
LOCK_NAMES = ("gpui", "gpui_apple", "gpui_macos", "gpui_platform")
FILES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", OTHER_MEMBER, MEMBER)


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
        (self.root / "rust-toolchain.toml").write_text(TOOLCHAIN)
        (self.root / MEMBER).parent.mkdir(parents=True)
        (self.root / MEMBER).write_text(MEMBER_MANIFEST)
        (self.root / OTHER_MEMBER).parent.mkdir(parents=True)
        (self.root / OTHER_MEMBER).write_text('[package]\nname = "fixture-core"\nversion = "0.1.0"\n')
        subprocess.run(["git", "add", *FILES], cwd=self.root, check=True)
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "fixture"],
            cwd=self.root,
            check=True,
        )
        self.remote = StubRemote()
        self.updates = []

    def update(self, root, packages):
        self.updates.append((root, packages))
        (root / "Cargo.lock").write_text(lockfile(NEW_TAG))

    def bump(self, tag=NEW_TAG):
        original_run = subprocess.run

        def run(command, **kwargs):
            if command[:2] == ["cargo", "metadata"]:
                self.assertEqual(command, ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"])
                packages = [
                    {"id": path, "manifest_path": str(self.root / path)}
                    for path in ("Cargo.toml", OTHER_MEMBER, MEMBER)
                ]
                metadata = {"packages": packages, "workspace_members": [item["id"] for item in packages]}
                return subprocess.CompletedProcess(command, 0, stdout=json.dumps(metadata), stderr="")
            return original_run(command, **kwargs)

        with patch.object(MODULE.subprocess, "run", run):
            return MODULE.bump(self.root, tag, self.remote, self.update)

    def snapshot(self):
        return tuple((self.root / name).read_bytes() for name in FILES)

    def assert_failure_preserves_files(self, kind):
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
        self.assertEqual(
            (self.root / MEMBER).read_text(), MEMBER_MANIFEST.replace(OLD_TAG, NEW_TAG)
        )
        self.assertEqual((self.root / "Cargo.lock").read_text(), lockfile(NEW_TAG))
        self.assertEqual((self.root / "rust-toolchain.toml").read_text(), TOOLCHAIN.replace("1.98.1", "1.99.0"))
        self.assertEqual(self.updates, [(self.root, list(LOCK_NAMES))])
        self.assertEqual(self.remote.queries, [("tag", NEW_TAG), ("toolchain", NEW_TAG)])

    def test_unstaged_change_refuses_bump(self):
        (self.root / "Cargo.toml").write_text(MANIFEST + "\n# local change\n")
        self.assert_failure_preserves_files(MODULE.Failure.DIRTY_INPUTS)
        self.assertEqual(self.remote.queries, [])

    def test_staged_change_refuses_bump(self):
        (self.root / "Cargo.lock").write_text(lockfile(OLD_TAG) + "\n# staged change\n")
        subprocess.run(["git", "add", "Cargo.lock"], cwd=self.root, check=True)
        self.assert_failure_preserves_files(MODULE.Failure.DIRTY_INPUTS)
        self.assertEqual(self.remote.queries, [])

    def test_staged_change_reversed_in_working_copy_refuses_bump(self):
        toolchain = self.root / "rust-toolchain.toml"
        toolchain.write_text(TOOLCHAIN.replace("1.98.1", "1.98.0"))
        subprocess.run(["git", "add", "rust-toolchain.toml"], cwd=self.root, check=True)
        toolchain.write_text(TOOLCHAIN)
        with self.assertRaises(MODULE.BumpError) as raised:
            self.bump()
        self.assertEqual(raised.exception.kind, MODULE.Failure.DIRTY_INPUTS)
        self.assertEqual(toolchain.read_text(), TOOLCHAIN)
        self.assertEqual(self.remote.queries, [])

    def test_member_manifest_change_refuses_bump(self):
        (self.root / MEMBER).write_text(MEMBER_MANIFEST + "\n# local change\n")
        self.assert_failure_preserves_files(MODULE.Failure.DIRTY_INPUTS)
        self.assertEqual(self.remote.queries, [])

    def test_disagreeing_manifest_tags_refuse_bump(self):
        (self.root / MEMBER).write_text(MEMBER_MANIFEST.replace(OLD_TAG, OLD_TAG + ".1"))
        subprocess.run(["git", "add", MEMBER], cwd=self.root, check=True)
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "different member tag"],
            cwd=self.root,
            check=True,
        )
        self.assert_failure_preserves_files(MODULE.Failure.TAGS_INCONSISTENT)
        self.assertEqual(self.remote.queries, [])

    def test_missing_tag_preserves_files(self):
        self.remote.exists = False
        self.assert_failure_preserves_files(MODULE.Failure.TAG_MISSING)
        self.assertEqual(self.remote.queries, [("tag", NEW_TAG)])

    def test_local_override_preserves_files(self):
        config = self.root / ".cargo" / "config.toml"
        config.parent.mkdir()
        config.write_text(f'[patch."{FORK_URL}"]\ngpui = {{ path = "/tmp/zed/gpui" }}\n')
        self.assert_failure_preserves_files(MODULE.Failure.OVERRIDE_ACTIVE)
        self.assertEqual(self.remote.queries, [])

    def test_unexpected_dependency_set_preserves_files(self):
        (self.root / "Cargo.toml").write_text(MANIFEST.replace("gpui_platform =", "different ="))
        subprocess.run(["git", "add", "Cargo.toml"], cwd=self.root, check=True)
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "fixture change"],
            cwd=self.root,
            check=True,
        )
        self.assert_failure_preserves_files(MODULE.Failure.DEPENDENCIES_UNEXPECTED)
        self.assertEqual(self.remote.queries, [])

    def test_remote_toolchain_without_channel_preserves_files(self):
        self.remote.contents = '[toolchain]\nprofile = "minimal"\n'
        self.assert_failure_preserves_files(MODULE.Failure.TOOLCHAIN_INVALID)

    def test_positive_numeric_suffixes_are_accepted(self):
        for tag in (f"{OLD_TAG}.1", f"{OLD_TAG}.12"):
            with self.subTest(tag=tag):
                def update(root, packages):
                    (root / "Cargo.lock").write_text(lockfile(tag))

                self.update = update
                result = self.bump(tag)
                self.assertEqual(result.new_tag, tag)
                self.assertEqual((self.root / "Cargo.lock").read_text(), lockfile(tag))
                subprocess.run(["git", "add", *FILES], cwd=self.root, check=True)
                subprocess.run(
                    ["git", "-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "fixture bump"],
                    cwd=self.root,
                    check=True,
                )

    def test_invalid_tags_are_rejected(self):
        original = self.snapshot()
        for tag in (
            f"{OLD_TAG}.0", f"{OLD_TAG}.01", f"{OLD_TAG}.001",
            f"{OLD_TAG}.-1", f"{OLD_TAG}.+1", f"{OLD_TAG}.1.2",
            f"{OLD_TAG}.", f"{OLD_TAG}.1a", f"{OLD_TAG}.١",
            "spaceterm-2026-9-26.1",
        ):
            with self.subTest(tag=tag):
                with self.assertRaises(MODULE.BumpError) as raised:
                    self.bump(tag)
                self.assertEqual(raised.exception.kind, MODULE.Failure.INVALID_TAG)
                self.assertEqual(self.snapshot(), original)
        self.assertEqual(self.remote.queries, [])

    def test_repeated_bump_is_idempotent(self):
        self.bump()
        subprocess.run(["git", "add", *FILES], cwd=self.root, check=True)
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "fixture bump"],
            cwd=self.root,
            check=True,
        )
        after_first = self.snapshot()
        result = self.bump()
        self.assertEqual(result, MODULE.BumpResult(NEW_TAG, NEW_TAG, "1.99.0", "1.99.0"))
        self.assertEqual(self.snapshot(), after_first)
        self.assertEqual(len(self.updates), 1)

    def test_failed_update_rolls_back_and_retry_succeeds(self):
        original = self.snapshot()
        successful_update = self.update

        def fail_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise MODULE.BumpError(MODULE.Failure.UPDATE_FAILED)

        self.update = fail_update
        with self.assertRaises(MODULE.BumpError) as raised:
            self.bump()
        self.assertEqual(raised.exception.kind, MODULE.Failure.UPDATE_FAILED)
        self.assertEqual(self.snapshot(), original)
        self.update = successful_update
        self.assertEqual(self.bump().new_tag, NEW_TAG)

    def test_toolchain_write_failure_rolls_back(self):
        original = self.snapshot()
        original_write = Path.write_text

        def fail_toolchain_write(path, contents, *args, **kwargs):
            if path == self.root / "rust-toolchain.toml":
                raise OSError("injected write failure")
            return original_write(path, contents, *args, **kwargs)

        with patch.object(Path, "write_text", fail_toolchain_write):
            with self.assertRaises(MODULE.BumpError) as raised:
                self.bump()
        self.assertEqual(raised.exception.kind, MODULE.Failure.FILE_WRITE_FAILED)
        self.assertEqual(self.snapshot(), original)

    def test_sigint_during_update_rolls_back(self):
        original = self.snapshot()

        def interrupt_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise KeyboardInterrupt

        self.update = interrupt_update
        with self.assertRaises(KeyboardInterrupt):
            self.bump()
        self.assertEqual(self.snapshot(), original)

    @unittest.skipUnless(os.name == "posix", "requires POSIX signals")
    def test_sigterm_during_update_rolls_back(self):
        original = self.snapshot()

        def interrupt_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            os.kill(os.getpid(), signal.SIGTERM)

        self.update = interrupt_update
        with self.assertRaises(MODULE.BumpCancelled):
            self.bump()
        self.assertEqual(self.snapshot(), original)

    def test_system_exit_during_update_rolls_back(self):
        original = self.snapshot()

        def interrupt_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise SystemExit(2)

        self.update = interrupt_update
        with self.assertRaises(SystemExit):
            self.bump()
        self.assertEqual(self.snapshot(), original)

    def test_failed_restore_reports_recovery_command_and_retry_succeeds(self):
        original = self.snapshot()
        successful_update = self.update

        def fail_update(root, packages):
            (root / "Cargo.lock").write_text("partial lockfile\n")
            raise MODULE.BumpError(MODULE.Failure.UPDATE_FAILED)

        self.update = fail_update
        original_run = subprocess.run
        restores = []

        def fail_restore(command, **kwargs):
            if command[:2] == ["git", "restore"]:
                restores.append(command)
                return subprocess.CompletedProcess(command, 1)
            return original_run(command, **kwargs)

        with patch.object(MODULE.subprocess, "run", fail_restore):
            with self.assertRaises(MODULE.BumpError) as raised:
                self.bump()
        self.assertEqual(raised.exception.kind, MODULE.Failure.RESTORE_FAILED)
        self.assertEqual(len(restores), 1)
        recovery_command = f"git restore --source=HEAD --staged --worktree -- {' '.join(FILES)}"
        self.assertIn(recovery_command, str(raised.exception))
        self.assertNotEqual(self.snapshot(), original)

        output = io.StringIO()
        with patch.object(sys, "argv", ["gpui-bump.py", NEW_TAG]):
            with patch.object(MODULE, "bump", side_effect=raised.exception):
                with redirect_stderr(output), self.assertRaises(SystemExit) as exit_result:
                    MODULE.main()
        self.assertNotEqual(exit_result.exception.code, 0)
        self.assertEqual(output.getvalue(), f"gpui:bump: rollback failed; run {recovery_command}\n")

        subprocess.run(recovery_command.split(), cwd=self.root, check=True)
        self.assertEqual(self.snapshot(), original)
        self.update = successful_update
        self.assertEqual(self.bump().new_tag, NEW_TAG)

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


if __name__ == "__main__":
    unittest.main()
