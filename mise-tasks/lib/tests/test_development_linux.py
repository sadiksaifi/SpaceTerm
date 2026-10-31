"""Exercise the Linux Development launcher in an isolated source tree."""

import json
import os
import plistlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import xml.etree.ElementTree as ElementTree
from pathlib import Path

from spaceterm_tasks import ROOT, server_package
from spaceterm_tasks.development_linux import APPLICATION_ID, ICON_DOCUMENT
from spaceterm_tasks.server_builds import _source

from tests.server_artifact_fixture import archive_tree, populate, seal_foreign

ICON_DOCUMENT = ROOT / ICON_DOCUMENT

# Cargo is the controlled external boundary. It reports a recording executable as its artifact.
FAKE_CARGO = """import json, os, pathlib, sys
arguments = sys.argv[1:]
assert not any(name in os.environ for name in (
    "SPACETERM_PACKAGED", "SPACETERM_RELEASE_TAG", "SPACETERM_CLI_ARCHIVE", "SPACETERM_CLI_SIGNATURE"
))
assert arguments[0] == "build" and "--locked" in arguments and "--features" not in arguments
binary = arguments[arguments.index("--bin") + 1]
executable = pathlib.Path.cwd() / "target" / "debug" / binary
executable.parent.mkdir(parents=True, exist_ok=True)
executable.write_text(
    "#!/bin/sh\\n"
    "if [ -n \\"${SPACETERM_TEST_PYTHONPATH:-}\\" ]; then\\n"
    "printf '%s\\\\n' \\"${PYTHONPATH-unset}\\" > \\"$SPACETERM_TEST_PYTHONPATH\\"\\n"
    "fi\\n"
    "printf '%s\\\\n' \\"$0\\" \\"${SPACETERM_DEVELOPER_WORKBENCH:-}\\" \\"$#\\" \\"$@\\" > \\"$SPACETERM_TEST_RECORD\\"\\n"
)
if binary == "spaceterm":
    executable.write_text(
        "#!/bin/sh\\nprintf '%s\\n' spaceterm-development-ready-v1\\ncat >/dev/null\\n"
    )
executable.chmod(0o755)
print(json.dumps({"reason": "compiler-artifact", "target": {"name": binary},
                  "executable": str(executable)}))
"""


# Catalog preparation keeps its real Git snapshot, signature and staging checks. Only
# compiling the two distribution binaries uses prepared artifacts in this launcher fixture.
CATALOG_DRIVER = """import json, os, pathlib, runpy, shutil, sys
from unittest.mock import patch
artifacts = json.loads(pathlib.Path(sys.argv[1]).read_text())
catalog = pathlib.Path(sys.argv[1]).with_name("native-catalog")
entry = catalog / "aarch64-apple-darwin"
entry.mkdir(parents=True, exist_ok=True)
archive, signature = map(pathlib.Path, artifacts["aarch64-apple-darwin"])
shutil.copyfile(archive, entry / "artifact.tar.gz")
shutil.copyfile(signature, entry / "artifact.sig")
import hashlib
(entry / "artifact.sha256").write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + "\\n")
os.environ["SPACETERM_CLI_ARTIFACTS"] = str(catalog)
sys.argv = sys.argv[2:]
def package(identity, *, target):
    assert identity == "spaceterm-development"
    return tuple(pathlib.Path(value) for value in artifacts[target])
with patch("spaceterm_tasks.server_builds.package", package):
    runpy.run_path(sys.argv[0], run_name="__main__")
"""


@unittest.skipUnless(sys.platform == "linux", "the private prefix is Linux-only")
class LinuxPrefixTests(unittest.TestCase):
    def setUp(self):
        for tool in ("tic", "desktop-file-validate", "git", "cc"):
            self.assertIsNotNone(shutil.which(tool), f"{tool} is required; run mise doctor project")
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.private = Path(directory.name)
        self.root = self.private / 'source space "quote" \\ slash %f =equal $dollar `tick`'
        self.root.mkdir()
        self.data = self.private / 'data space "quote" \\ slash %f'
        self.home = self.private / "home"
        self.home.mkdir()
        for directory in (
            "mise-tasks",
            "packaging/linux",
            "assets/shell-integration",
            "assets/terminfo",
            ICON_DOCUMENT.relative_to(ROOT),
        ):
            shutil.copytree(
                ROOT / directory,
                self.root / directory,
                ignore=shutil.ignore_patterns("__pycache__"),
            )
        for name in ("server-nonproduction-trust.json", "server-resources.json"):
            shutil.copyfile(ROOT / "packaging" / name, self.root / "packaging" / name)
        (self.root / ".gitignore").write_text("target/\nrecord\n__pycache__/\n")
        for arguments in (
            ("init", "-q"),
            ("config", "user.name", "Fixture"),
            ("config", "user.email", "fixture@example.invalid"),
            ("add", "."),
            ("-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture"),
        ):
            subprocess.run(["git", "-C", str(self.root), *arguments], check=True)
        self.artifacts = self.private / "server-packages.json"
        self.artifacts.write_text(json.dumps(self.server_artifacts()))
        self.driver = self.private / "catalog-driver.py"
        self.driver.write_text(CATALOG_DRIVER)
        self.commands = self.private / "commands"
        self.commands.mkdir()
        (self.commands / "cargo").write_text(f"#!{sys.executable}\n" + FAKE_CARGO)
        (self.commands / "cargo").chmod(0o755)
        self.record = self.root / "record"

    def server_artifacts(self):
        artifacts = {}
        for target in server_package.TARGETS.values():
            tree = self.private / "server-packages" / target / "server"
            executable = tree / server_package.executable_path(target, "spaceterm-development")
            executable.parent.mkdir(parents=True)
            info = {
                "schema": 1,
                "source_sha256": _source(self.root)[2],
                "identity": "spaceterm-development",
                "target": target,
                "commit": "a" * 40,
                "version": "dev.aaaaaaaaaaaa",
                "protocol_major": server_package.PROTOCOL_MAJOR,
            }
            if target == "x86_64-unknown-linux-gnu":
                source = f"""
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {{
    if (argc == 4 && strcmp(argv[1], "server") == 0 && strcmp(argv[2], "provision") == 0 && strcmp(argv[3], "build-info") == 0) {{
        puts({json.dumps(json.dumps(info))});
        return 0;
    }}
    if (argc == 4 && strcmp(argv[1], "server") == 0 && strcmp(argv[2], "provision") == 0 && strcmp(argv[3], "check") == 0) {{
        puts("Server checks passed.");
        return 0;
    }}
    return 1;
}}
"""
                subprocess.run(
                    ["cc", "-x", "c", "-", "-o", str(executable)],
                    input=source,
                    text=True,
                    capture_output=True,
                    check=True,
                )
            else:
                executable.write_bytes(
                    bytes.fromhex("cffaedfe0c0000010000000002000000") + bytes(16)
                )
                executable.chmod(0o755)
            populate(ROOT, tree, info)
            seal_foreign(tree, info)
            artifacts[target] = [str(path) for path in archive_tree(ROOT, tree, info)]
        return artifacts

    def environment(self):
        environment = dict(os.environ)
        for key in ("DISPLAY", "WAYLAND_DISPLAY", "SESSION_MANAGER", "KDE_SESSION_VERSION"):
            environment.pop(key, None)
        environment.update(
            {
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
                "PATH": str(self.commands) + os.pathsep + os.environ["PATH"],
                "PYTHONPATH": str(self.root / "mise-tasks" / "lib"),
            }
        )
        return environment

    def launcher(self, *arguments):
        return [
            sys.executable,
            str(self.driver),
            str(self.artifacts),
            str(self.root / "mise-tasks" / "development" / "linux.py"),
            *arguments,
        ]

    def run_development(self, *arguments, section="", overrides=None):
        environment = self.environment()
        environment["SPACETERM_DEVELOPER_WORKBENCH"] = section
        environment.update(overrides or {})
        return subprocess.run(
            self.launcher(*arguments),
            env=environment,
            capture_output=True,
            text=True,
            cwd=self.root,
        )

    def test_development_launches_from_a_private_prefix_with_installed_resources(self):
        result = self.run_development()
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = self.root / "target" / "development-apps" / "development"
        self.assertEqual(
            self.record.read_text().splitlines(),
            [str(prefix / "bin" / "spaceterm-desktop"), "", "1", "--new-instance"],
            "a source launch runs beside a running Development instance",
        )
        self.assertTrue((prefix / "share" / "spaceterm" / "shell-integration" / "zsh").is_dir())
        self.assertTrue(
            (prefix / "share" / "spaceterm" / "terminfo" / "x" / "xterm-spaceterm").is_file()
        )
        self.assertEqual(
            [path.name for path in prefix.parent.iterdir()],
            ["development"],
            "the staging directory must be renamed into place",
        )

    def test_source_launch_ignores_packaging_and_signed_artifact_overrides(self):
        result = self.run_development(
            overrides={
                "SPACETERM_PACKAGED": "1",
                "SPACETERM_RELEASE_TAG": "v1.0.0",
                "SPACETERM_CLI_ARCHIVE": "/absent/archive",
                "SPACETERM_CLI_SIGNATURE": "/absent/signature",
            }
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.record.exists())

    def test_development_registers_its_desktop_identity_and_replaces_only_its_entry(self):
        applications = self.data / "applications"
        applications.mkdir(parents=True)
        entry = applications / (APPLICATION_ID + ".desktop")
        entry.write_text("stale development metadata")
        unrelated = applications / "unrelated.desktop"
        unrelated.write_text("unrelated metadata")
        result = self.run_development()
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
            geometry = {
                key: value for key, value in element.attrib.items() if key not in ("fill", "stroke")
            }
            return element.tag, tuple(sorted(geometry.items()))

        drawn = {shape(element): element for element in artwork.iter()}
        for layer in (ICON_DOCUMENT / "Assets").iterdir():
            for element in list(ElementTree.parse(layer).getroot()):
                self.assertIn(shape(element), drawn, layer.name)
                for paint in ("fill", "stroke"):
                    if element.get(paint) not in (None, "none"):
                        self.assertEqual(drawn[shape(element)].get(paint), "#FFFFFF")
        parsed = subprocess.run(
            ["desktop-file-validate", str(entry)],
            env=self.environment(),
            capture_output=True,
            text=True,
        )
        self.assertEqual(parsed.returncode, 0, parsed.stderr + parsed.stdout)

    def test_relative_or_empty_xdg_data_home_uses_the_private_home_registry(self):
        for value in ("relative-data", ""):
            with self.subTest(value=value):
                result = self.run_development(overrides={"XDG_DATA_HOME": value})
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
        result = self.run_development()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(entry.is_symlink())
        self.assertEqual(unrelated.read_text(), "retain me")

    def test_failed_validation_preserves_the_installed_entry_and_does_not_launch(self):
        applications = self.data / "applications"
        applications.mkdir(parents=True)
        entry = applications / (APPLICATION_ID + ".desktop")
        entry.write_text("retain previous metadata")
        validator = self.commands / "desktop-file-validate"
        validator.write_text("#!/bin/sh\nprintf '%s\\n' private-diagnostic >&2\nexit 1\n")
        validator.chmod(0o755)
        result = self.run_development()
        self.assertEqual(result.returncode, 1)
        self.assertFalse(self.record.exists())
        self.assertEqual(entry.read_text(), "retain previous metadata")
        self.assertNotIn("private-diagnostic", result.stderr)

    def test_registered_exec_forwards_an_invitation_through_gio_with_reserved_path_characters(self):
        self.assertTrue(
            Path("/usr/bin/python3").is_file(),
            "system Python with python3-gi is required; run mise doctor project",
        )
        probe = subprocess.run(
            ["/usr/bin/python3", "-c", "from gi.repository import Gio; assert Gio.DesktopAppInfo"],
            env=self.environment(),
            capture_output=True,
        )
        self.assertEqual(
            probe.returncode,
            0,
            "GIO desktop parsing needs python3-gi and gir1.2-glib-2.0; run mise doctor project",
        )
        result = self.run_development()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.record.unlink()
        entry = self.data / "applications" / (APPLICATION_ID + ".desktop")
        invitation = "spaceterm://pair/development-invitation"
        launch = subprocess.run(
            [
                "/usr/bin/python3",
                "-c",
                """
import json, sys
from gi.repository import Gio
app = Gio.DesktopAppInfo.new_from_filename(sys.argv[1])
assert app is not None
print(json.dumps({"id": app.get_id(), "name": app.get_name()}))
assert app.launch_uris([sys.argv[2]], None)
""",
                str(entry),
                invitation,
            ],
            env=self.environment(),
            capture_output=True,
            text=True,
        )
        self.assertEqual(launch.returncode, 0, launch.stderr)
        self.assertEqual(
            json.loads(launch.stdout),
            {
                "id": APPLICATION_ID + ".desktop",
                "name": "SpaceTerm Development",
            },
        )
        deadline = time.monotonic() + 5
        while not self.record.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertEqual(
            self.record.read_text().splitlines(),
            [
                str(self.root / "target/development-apps/development/bin/spaceterm-desktop"),
                "",
                "1",
                invitation,
            ],
            "desktop URL delivery preserves the invitation for the running instance",
        )

    def test_development_launch_preserves_the_workbench_request(self):
        result = self.run_development(section="controls")
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = self.root / "target" / "development-apps" / "development"
        self.assertEqual(
            self.record.read_text().splitlines(),
            [str(prefix / "bin" / "spaceterm-desktop"), "controls", "1", "--new-instance"],
        )

    def test_development_launch_removes_only_the_task_library_from_pythonpath(self):
        library = str(self.root / "mise-tasks" / "lib")
        observed = self.private / "pythonpath"
        for pythonpath, expected in (
            (library, "unset"),
            (os.pathsep.join(["/developer", library]), "/developer"),
        ):
            with self.subTest(pythonpath=pythonpath):
                result = self.run_development(
                    overrides={"PYTHONPATH": pythonpath, "SPACETERM_TEST_PYTHONPATH": str(observed)}
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(observed.read_text(), expected + "\n")

    def test_sigterm_exits_after_removing_the_staging_prefix(self):
        ready = self.private / "tic-ready"
        release = self.private / "tic-release"
        tic = self.commands / "tic"
        tic.write_text(
            f"#!{sys.executable}\n"
            + """import os, pathlib, sys, time
arguments = sys.argv[1:]
output = pathlib.Path(arguments[arguments.index("-o") + 1]) / "x/xterm-spaceterm"
output.parent.mkdir(parents=True)
output.touch()
pathlib.Path(os.environ["SPACETERM_TEST_READY"]).touch()
while not pathlib.Path(os.environ["SPACETERM_TEST_RELEASE"]).exists():
    time.sleep(0.01)
"""
        )
        tic.chmod(0o755)
        environment = self.environment()
        environment.update(SPACETERM_TEST_READY=str(ready), SPACETERM_TEST_RELEASE=str(release))
        process = subprocess.Popen(
            self.launcher(),
            env=environment,
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 5
            while not ready.exists() and process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(ready.exists(), "the controlled tic must reach the staging boundary")
            process.send_signal(signal.SIGTERM)
            release.touch()
            _, error = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 128 + signal.SIGTERM, error)
            self.assertFalse(self.record.exists())
            self.assertEqual(list((self.root / "target/development-apps").iterdir()), [])
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=5)

    def test_unknown_argument_is_rejected_before_building(self):
        result = self.run_development("release")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "target").exists())
        self.assertIn("unrecognized arguments", result.stderr)


if __name__ == "__main__":
    unittest.main()
