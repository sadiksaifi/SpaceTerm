"""Check the task catalog's conventions and the bundle metadata packaging duplicates."""

import os
import plistlib
import re
import shlex
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

    def cargo_commands(self, task):
        """Resolve TOML task calls without mistaking dependency builds for package coverage."""
        for entry in run_entries(self.config["tasks"].get(task, {})):
            if isinstance(entry, dict):
                yield from self.cargo_commands(entry["task"])
            elif entry.startswith("cargo "):
                yield shlex.split(entry)

    def test_validation_gates_check_lint_and_test_every_headless_package(self):
        packages = {
            "spaceterm-core",
            "spaceterm-protocol",
            "spaceterm-client",
            "spaceterm-client-native",
            "spaceterm-host",
            "spaceterm-server",
            "spaceterm-distribution",
        }
        for platform in (*PLATFORMS, "portable"):
            coverage = {action: set() for action in ("test", "check", "clippy")}
            for command in self.cargo_commands(f"validate:{platform}"):
                action = command[1]
                if action not in coverage or "--all-targets" not in command:
                    continue
                if any(flag in command for flag in ("--test", "--bin", "--lib", "--doc")):
                    continue
                if action == "clippy" and "--no-deps" not in command:
                    continue
                selected = {
                    command[index + 1]
                    for index, token in enumerate(command[:-1])
                    if token in ("--package", "-p")
                }
                coverage[action].update(selected)
            for action, selected in coverage.items():
                with self.subTest(platform=platform, action=action):
                    self.assertFalse(
                        packages - selected, f"Missing packages: {packages - selected}"
                    )

    def test_validation_gates_include_cli_and_storage_test_features(self):
        for platform in (*PLATFORMS, "portable"):
            commands = list(self.cargo_commands(f"validate:{platform}"))
            for package, feature in (
                ("spaceterm-client-native", "cli"),
                ("spaceterm-host", "test-support"),
                ("spaceterm-client", "test-support"),
            ):
                with self.subTest(platform=platform, package=package):
                    self.assertTrue(
                        any(
                            command[1] == "test"
                            and package in command
                            and "--all-targets" in command
                            and "--features" in command
                            and feature in command[command.index("--features") + 1].split(",")
                            for command in commands
                        ),
                        f"Missing {package} tests with {feature}",
                    )

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
