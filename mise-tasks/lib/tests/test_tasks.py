"""Check the task catalog's conventions and the bundle metadata packaging duplicates."""

import os
import plistlib
import re
import tomllib
import unittest

from spaceterm_tasks import ROOT

TASKS = ROOT / "mise-tasks"
PLATFORMS = ("macos", "linux")
DISPATCH = re.compile(r"^(.+):\{\{ os\(\) \}\}$")


def file_tasks():
    """Map each file task name to its path, as mise names them."""
    tasks = {}
    for path in TASKS.rglob("*.py"):
        relative = path.relative_to(TASKS)
        if relative.parts[0] != "lib":
            tasks[":".join(relative.with_suffix("").parts)] = path
    return tasks


def run_entries(definition):
    run = definition.get("run", [])
    return run if isinstance(run, list) else [run]


class TaskCatalogTests(unittest.TestCase):
    def setUp(self):
        self.config = tomllib.loads((ROOT / ".mise.toml").read_text())
        self.files = file_tasks()
        self.names = set(self.config["tasks"]) | set(self.files)

    def test_every_dispatch_reaches_a_task_on_each_platform(self):
        dispatched = set()
        for definition in self.config["tasks"].values():
            for entry in run_entries(definition):
                match = DISPATCH.match(entry.get("task", "")) if isinstance(entry, dict) else None
                if match:
                    dispatched.add(match[1])
        self.assertTrue(dispatched)
        for name in dispatched:
            for platform in PLATFORMS:
                with self.subTest(task=name, platform=platform):
                    self.assertIn(f"{name}:{platform}", self.names)

    def test_every_referenced_task_exists(self):
        for name, definition in self.config["tasks"].items():
            for entry in run_entries(definition):
                if isinstance(entry, dict) and not DISPATCH.match(entry["task"]):
                    with self.subTest(task=name):
                        self.assertIn(entry["task"], self.names)

    def test_file_tasks_are_executable_and_described_and_library_code_is_not(self):
        for name, path in self.files.items():
            with self.subTest(task=name):
                self.assertTrue(os.access(path, os.X_OK))
                self.assertIn("\n# MISE description=", path.read_text())
        for path in (TASKS / "lib").rglob("*.py"):
            with self.subTest(path=path.name):
                self.assertFalse(os.access(path, os.X_OK))

    def test_toml_tasks_are_described(self):
        for name, definition in self.config["tasks"].items():
            with self.subTest(task=name):
                self.assertTrue(definition.get("description"))


class PackagerConfigTests(unittest.TestCase):
    def test_packager_config_matches_its_identity_property_list(self):
        for identity in ("preflight", "spaceterm"):
            with self.subTest(identity=identity):
                directory = ROOT / "packaging" / "macos" / identity
                config = tomllib.loads((directory / "Packager.toml").read_text())
                info = plistlib.loads((directory / "Info.plist").read_bytes())
                self.assertEqual(config["product-name"], info["CFBundleName"])
                self.assertEqual(config["identifier"], info["CFBundleIdentifier"])
                self.assertEqual(
                    config["binaries"], [{"path": info["CFBundleExecutable"], "main": True}]
                )
                self.assertEqual(
                    config["macos"]["minimum-system-version"], info["LSMinimumSystemVersion"]
                )
                self.assertEqual(
                    config["macos"]["entitlements"],
                    f"packaging/macos/{identity}/Entitlements.plist",
                )


if __name__ == "__main__":
    unittest.main()
