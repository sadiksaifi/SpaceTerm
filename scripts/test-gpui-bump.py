#!/usr/bin/env python3
"""Exercise GPUI fork bumps in an isolated repository without network access."""

import importlib.util
from pathlib import Path
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

    def test_cargo_update_scopes_the_command_to_lockfile_packages(self):
        with patch.object(MODULE.subprocess, "run") as run:
            MODULE.cargo_update(self.root, list(LOCK_NAMES))
        command = ["cargo", "update"]
        for name in LOCK_NAMES:
            command.extend(("--package", name))
        run.assert_called_once_with(
            command, cwd=self.root, check=True, capture_output=True, text=True
        )


if __name__ == "__main__":
    unittest.main()
