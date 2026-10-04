#!/usr/bin/env python3
"""Test development-app dispatch and the Linux private-prefix staging script."""

import contextlib
import io
import importlib.util
import json
import os
import plistlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ElementTree
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
SPEC = importlib.util.spec_from_file_location("run_platform_task", SCRIPTS / "run-platform-task.py")
DISPATCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DISPATCH)
PLATFORM_TASKS = {
    "darwin": {"development", "development:updates", "development:workbench"},
    "linux": {"development", "development:workbench"},
}
PLATFORM_SEGMENT = {"darwin": "macos", "linux": "linux"}
APPLICATION_ID = "io.github.sadiksaifi.spaceterm-development"
ICON_DOCUMENT = ROOT / "packaging" / "macos" / "development" / "SpaceTerm Development.icon"


class DispatchTests(unittest.TestCase):
    def test_every_generic_task_dispatches_only_to_available_platform_variants(self):
        tasks = tomllib.loads((ROOT / ".mise.toml").read_text())["tasks"]
        generic = {
            name: shlex.split(definition["run"])[2]
            for name, definition in tasks.items()
            if isinstance(definition.get("run"), str)
            and "scripts/run-platform-task.py" in definition["run"]
        }
        self.assertEqual(set(generic), set.union(*PLATFORM_TASKS.values()))
        for platform, segment in PLATFORM_SEGMENT.items():
            available = {name for name in generic if f"{name}:{segment}" in tasks}
            self.assertEqual(available, PLATFORM_TASKS[platform], platform)
            for name, profile in generic.items():
                with self.subTest(platform=platform, task=name):
                    self.assertEqual(profile, name)
                    implementation = f"{profile}:{segment}"
                    error = io.StringIO()
                    with patch.object(DISPATCH.sys, "platform", platform), \
                            patch.object(DISPATCH.os, "execvp") as execute, \
                            contextlib.redirect_stderr(error):
                        status = DISPATCH.main([profile, "preview argument"])
                    if name in PLATFORM_TASKS[platform]:
                        execute.assert_called_once_with(
                            "mise", ["mise", "run", implementation, "preview argument"])
                    else:
                        execute.assert_not_called()
                        self.assertEqual(status, 2)
                        self.assertIn("task_not_available_on_platform", error.getvalue())
                        self.assertIn("not available on this platform", error.getvalue())
                        with self.assertRaises(DISPATCH.TaskUnavailable):
                            DISPATCH.platform_task(profile, platform)

    def test_unsupported_platform_reports_unavailability_without_dispatching(self):
        error = io.StringIO()
        with patch.object(DISPATCH.sys, "platform", "win32"), \
                patch.object(DISPATCH.os, "execvp") as execute, \
                contextlib.redirect_stderr(error):
            self.assertEqual(DISPATCH.main(["development"]), 2)
        execute.assert_not_called()
        self.assertIn("task_not_available_on_platform", error.getvalue())


FAKE_ARTIFACTS = """#!/bin/sh
[ "$1" = run ] && [ "$2" = -- ] || exit 97
shift 2
exec "$@"
"""

FAKE_BUILD = """import pathlib, sys
arguments = sys.argv[1:]
output = pathlib.Path(arguments[arguments.index("--output") + 1])
assert "--locked" in arguments
assert "--features" not in arguments
executable = pathlib.Path(__file__).resolve().parent.parent / "target" / "debug" / "spaceterm"
executable.parent.mkdir(parents=True, exist_ok=True)
executable.write_text(
    "#!/bin/sh\\n"
    f"printf '%s\\\\n' \\"$0\\" \\"${{SPACETERM_DEVELOPER_WORKBENCH:-}}\\" \\"$#\\" \\"$@\\" > \\"$SPACETERM_TEST_RECORD\\"\\n"
)
executable.chmod(0o755)
output.write_text(str(executable))
"""


@unittest.skipUnless(
    sys.platform == "linux" and shutil.which("tic") and shutil.which("desktop-file-validate"),
    "the Linux prefix needs GNU tools, tic, and desktop-file-utils",
)
class LinuxPrefixTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.private = Path(directory.name)
        self.root = self.private / 'source space "quote" \\ slash %f =equal $dollar `tick`'
        self.root.mkdir()
        self.data = self.private / 'data space "quote" \\ slash %f'
        self.home = self.private / "home"
        self.home.mkdir()
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copy2(SCRIPTS / "run-development-app-linux.sh", scripts)
        shutil.copy2(SCRIPTS / "development-desktop-linux.py", scripts)
        (scripts / "cargo-artifacts.sh").write_text(FAKE_ARTIFACTS)
        (scripts / "cargo-artifacts.sh").chmod(0o755)
        (scripts / "build-cargo-executable.py").write_text(FAKE_BUILD)
        shutil.copytree(ROOT / "assets" / "shell-integration", self.root / "assets" / "shell-integration")
        shutil.copytree(ROOT / "assets" / "terminfo", self.root / "assets" / "terminfo")
        shutil.copytree(ICON_DOCUMENT, self.root / ICON_DOCUMENT.relative_to(ROOT))
        self.record = self.root / "record"

    def environment(self):
        environment = dict(os.environ)
        for key in ("DISPLAY", "WAYLAND_DISPLAY", "SESSION_MANAGER", "KDE_SESSION_VERSION"):
            environment.pop(key, None)
        environment.update({
            "HOME": str(self.home),
            "XDG_DATA_HOME": str(self.data),
            "XDG_CONFIG_HOME": str(self.private / "config"),
            "XDG_CACHE_HOME": str(self.private / "cache"),
            "XDG_STATE_HOME": str(self.private / "state"),
            "XDG_RUNTIME_DIR": str(self.private / "runtime"),
            "XDG_DATA_DIRS": str(self.private / "system-data"),
            "XDG_CONFIG_DIRS": str(self.private / "system-config"),
            "XDG_CURRENT_DESKTOP": "GNOME",
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=" + str(self.private / "absent-bus"),
            "SPACETERM_TEST_RECORD": str(self.record),
            "TMPDIR": str(self.private),
        })
        return environment

    def run_profile(self, *arguments, section="", overrides=None):
        environment = self.environment()
        environment["SPACETERM_DEVELOPER_WORKBENCH"] = section
        environment.update(overrides or {})
        return subprocess.run(
            ["bash", str(self.root / "scripts" / "run-development-app-linux.sh"), *arguments],
            env=environment,
            capture_output=True,
            text=True,
            cwd=self.root,
        )

    def test_dev_profile_launches_from_a_private_prefix_with_installed_resources(self):
        result = self.run_profile()
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = self.root / "target" / "development-apps" / "development"
        self.assertEqual(
            self.record.read_text().splitlines(),
            [str(prefix / "bin" / "spaceterm"), "", "1", "--new-instance"],
            "a source launch runs beside a running Development instance",
        )
        self.assertTrue((prefix / "share" / "spaceterm" / "shell-integration" / "zsh").is_dir())
        self.assertTrue((prefix / "share" / "spaceterm" / "terminfo" / "x" / "xterm-spaceterm").is_file())
        self.assertEqual(
            [path.name for path in prefix.parent.iterdir()],
            ["development"],
            "the staging directory must be renamed into place",
        )

    def test_development_registers_its_desktop_identity_and_replaces_only_its_entry(self):
        applications = self.data / "applications"
        applications.mkdir(parents=True)
        entry = applications / (APPLICATION_ID + ".desktop")
        entry.write_text("stale development metadata")
        unrelated = applications / "unrelated.desktop"
        unrelated.write_text("unrelated metadata")
        result = self.run_profile()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(unrelated.read_text(), "unrelated metadata")
        contents = entry.read_text()
        identity = plistlib.loads((ROOT / "packaging/macos/development/Info.plist").read_bytes())
        self.assertEqual(identity["CFBundleIdentifier"], APPLICATION_ID)
        self.assertIn("Name=" + identity["CFBundleDisplayName"] + "\n", contents)
        self.assertIn("Name=SpaceTerm Development\n", contents)
        self.assertIn("Type=Application\n", contents)
        self.assertIn("StartupWMClass=" + APPLICATION_ID + "\n", contents)
        self.assertIn("Terminal=false\n", contents)
        icon = self.root / "target/development-apps/development/share/icons/hicolor/scalable/apps"
        icon = icon / (APPLICATION_ID + ".svg")
        self.assertIn("Icon=" + str(icon).replace("\\", "\\\\") + "\n", contents)
        artwork = ElementTree.parse(icon).getroot()
        self.assertEqual(artwork.tag, "{http://www.w3.org/2000/svg}svg")
        self.assertEqual(artwork.get("viewBox"), "0 0 1024 1024")
        def shape(element):
            geometry = {key: value for key, value in element.attrib.items() if key not in ("fill", "stroke")}
            return element.tag, tuple(sorted(geometry.items()))

        drawn = {shape(element): element for element in artwork.iter()}
        for layer in (ICON_DOCUMENT / "Assets").iterdir():
            for element in list(ElementTree.parse(layer).getroot()):
                self.assertIn(shape(element), drawn, layer.name)
                for paint in ("fill", "stroke"):
                    if element.get(paint) not in (None, "none"):
                        self.assertEqual(drawn[shape(element)].get(paint), "#FFFFFF")
        if shutil.which("desktop-file-validate"):
            parsed = subprocess.run(
                ["desktop-file-validate", str(entry)], env=self.environment(), capture_output=True, text=True,
            )
            self.assertEqual(parsed.returncode, 0, parsed.stderr + parsed.stdout)

    def test_relative_or_empty_xdg_data_home_uses_the_private_home_registry(self):
        for value in ("relative-data", ""):
            with self.subTest(value=value):
                result = self.run_profile(overrides={"XDG_DATA_HOME": value})
                self.assertEqual(result.returncode, 0, result.stderr)
                entry = self.home / ".local/share/applications" / (APPLICATION_ID + ".desktop")
                self.assertTrue(entry.is_file())
                self.assertFalse((self.root / "relative-data").exists())

    def test_replacing_own_symlink_does_not_modify_its_target(self):
        applications = self.data / "applications"
        applications.mkdir(parents=True)
        unrelated = self.private / "unrelated"
        unrelated.write_text("retain me")
        entry = applications / (APPLICATION_ID + ".desktop")
        entry.symlink_to(unrelated)
        result = self.run_profile()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(entry.is_symlink())
        self.assertEqual(unrelated.read_text(), "retain me")

    def test_failed_validation_preserves_the_installed_entry_and_does_not_launch(self):
        applications = self.data / "applications"
        applications.mkdir(parents=True)
        entry = applications / (APPLICATION_ID + ".desktop")
        entry.write_text("retain previous metadata")
        commands = self.private / "commands"
        commands.mkdir()
        validator = commands / "desktop-file-validate"
        validator.write_text("#!/bin/sh\nprintf '%s\\n' private-diagnostic >&2\nexit 1\n")
        validator.chmod(0o755)
        result = self.run_profile(overrides={"PATH": str(commands) + os.pathsep + os.environ["PATH"]})
        self.assertEqual(result.returncode, 1)
        self.assertFalse(self.record.exists())
        self.assertEqual(entry.read_text(), "retain previous metadata")
        self.assertNotIn("private-diagnostic", result.stderr)

    @unittest.skipUnless(Path("/usr/bin/python3").exists(), "GIO parsing uses system Python")
    def test_registered_exec_round_trips_reserved_characters_through_gio(self):
        probe = subprocess.run(
            ["/usr/bin/python3", "-c", "from gi.repository import Gio; assert Gio.DesktopAppInfo"],
            env=self.environment(),
            capture_output=True,
        )
        if probe.returncode:
            self.skipTest("Python GIO desktop parser is unavailable")
        result = self.run_profile()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.record.unlink()
        entry = self.data / "applications" / (APPLICATION_ID + ".desktop")
        launch = subprocess.run(
            ["/usr/bin/python3", "-c", """
import json, sys
from gi.repository import Gio
app = Gio.DesktopAppInfo.new_from_filename(sys.argv[1])
assert app is not None
print(json.dumps({"id": app.get_id(), "name": app.get_name()}))
assert app.launch([], None)
""", str(entry)],
            env=self.environment(), capture_output=True, text=True,
        )
        self.assertEqual(launch.returncode, 0, launch.stderr)
        self.assertEqual(json.loads(launch.stdout), {
            "id": APPLICATION_ID + ".desktop", "name": "SpaceTerm Development",
        })
        deadline = time.monotonic() + 5
        while not self.record.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertEqual(self.record.read_text().splitlines(), [
            str(self.root / "target/development-apps/development/bin/spaceterm"), "", "0",
        ], "a desktop launch activates a running instance")

    def test_development_launch_preserves_the_workbench_request(self):
        result = self.run_profile(section="controls")
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = self.root / "target" / "development-apps" / "development"
        self.assertEqual(
            self.record.read_text().splitlines(),
            [str(prefix / "bin" / "spaceterm"), "controls", "1", "--new-instance"],
        )

    def test_unknown_profile_is_rejected_before_building(self):
        result = self.run_profile("release")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "target").exists())


if __name__ == "__main__":
    unittest.main()
