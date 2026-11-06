#!/usr/bin/env python
# MISE description="Run the release installer against a served fixture release on macOS"
"""Run install.sh against a served fixture release, real code signatures and a real disk image."""

import json
import os
import plistlib
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks.release import RELEASES
from tests.installer_fixture import (
    CORE,
    CURL,
    TEAM_ID,
    TOKEN,
    Release,
    build_document,
    core_archive,
    fake,
    render,
    run_with_tty,
    sha256,
)

PLATFORM = "macos-arm64"
CORE_RELATIVE = "SpaceTerm Core.app/Contents/MacOS/spaceterm"
# Code signatures cover Mach-O executables, so the fake Core is a native wrapper that runs the
# sealed script in its bundle's Resources.
CORE_WRAPPER = r"""
#include <libgen.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    char copy[PATH_MAX], script[PATH_MAX];
    strlcpy(copy, argv[0], sizeof copy);
    snprintf(script, sizeof script, "%s/../Resources/core.sh", dirname(copy));
    char *arguments[argc + 2];
    arguments[0] = "sh";
    arguments[1] = script;
    for (int index = 1; index < argc; index++) arguments[index + 1] = argv[index];
    arguments[argc + 1] = NULL;
    execv("/bin/sh", arguments);
    return 127;
}
"""
# A native executable keeps its bundle path in its arguments, which the installer matches.
DESKTOP = "#include <unistd.h>\nint main(void) { for (;;) pause(); }\n"
# Download URLs that start the installed app, as a person might during the download.
LAUNCHING_CURL = (
    CURL
    + """
if url.endswith(".dmg") and os.environ.get("LAUNCH_DURING_DOWNLOAD"):
    import subprocess
    process = subprocess.Popen([os.environ["LAUNCH_DURING_DOWNLOAD"]], start_new_session=True,
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL)
    with open(os.environ["LAUNCHED"], "w") as record:
        record.write(str(process.pid))
"""
)
VALID_KEY = "A" * 43 + "="


def core_name(version):
    return f"spaceterm-core-{version}-{PLATFORM}.tar.gz"


def image_name(version):
    return f"SpaceTerm-{version}-{PLATFORM}.dmg"


@unittest.skipUnless(sys.platform == "darwin", "the macOS installer runs only on macOS")
class InstallTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="spaceterm-install-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.home = self.root / "home"
        self.home.mkdir()
        # Brackets and spaces are literal path characters, not process-match syntax.
        self.applications = self.root / "Applications [personal]"
        self.temporary = self.root / "tmp"
        self.temporary.mkdir()
        self.requests = self.root / "requests.log"
        self.core_log = self.root / "core.log"
        self.release = Release(self.root)

        # The network is the controlled external boundary. Code signing, disk image mounting,
        # copying, and process checks use the real macOS tools.
        self.bin = self.root / "bin"
        self.bin.mkdir()
        fake(self.bin, "curl", LAUNCHING_CURL)
        self.compiled = {}
        for name, source in (("core", CORE_WRAPPER), ("desktop", DESKTOP)):
            executable = self.root / f"{name}-executable"
            subprocess.run(
                ["cc", "-x", "c", "-", "-o", executable],
                input=source,
                text=True,
                check=True,
                capture_output=True,
            )
            self.compiled[name] = executable
        self.installer = render(self.root, kind="dry-run", team_id="")
        self.publish("1.2.0", desktop=True)

    # Fixtures.

    def bundle(self, path, executable, name, identifier, version=None, sign=True):
        (path / "Contents/MacOS").mkdir(parents=True)
        shutil.copyfile(self.compiled[executable], path / "Contents/MacOS" / name)
        (path / "Contents/MacOS" / name).chmod(0o755)
        info = {"CFBundleExecutable": name, "CFBundleIdentifier": identifier}
        if version:
            info["CFBundleShortVersionString"] = version
        (path / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        if executable == "core":
            (path / "Contents/Resources").mkdir()
            (path / "Contents/Resources/core.sh").write_text(CORE)
        if sign:
            subprocess.run(
                ["codesign", "--sign", "-", "--identifier", identifier, path],
                check=True,
                capture_output=True,
            )
        return path

    def core(self, version, sign=True, tamper=False, floor="1.0.0"):
        directory = self.root / f"core-{version}-{sign}-{tamper}-{floor}"
        if directory.exists():
            shutil.rmtree(directory)
        tree = directory / "core"
        bundle = self.bundle(
            tree / "SpaceTerm Core.app",
            "core",
            "spaceterm",
            "com.peblon.spaceterm.core",
            sign=sign,
        )
        if tamper:
            with (bundle / "Contents/Resources/core.sh").open("a") as script:
                script.write("# changed after signing\n")
        archive = core_archive(
            directory / core_name(version),
            version,
            tree=tree,
            document=build_document(version, floor=floor),
        )
        signature = directory / f"{archive.name}.sig"
        signature.write_bytes(bytes(64))
        return archive, signature

    def disk_image(self, version, sign=True, tamper=False):
        source = self.root / f"disk-{version}-{sign}-{tamper}"
        if source.exists():
            shutil.rmtree(source)
        application = self.bundle(
            source / "SpaceTerm.app",
            "desktop",
            "SpaceTerm",
            "com.peblon.spaceterm",
            version,
            sign,
        )
        if tamper:
            (application / "Contents/Resources").mkdir()
            (application / "Contents/Resources/added").write_text("added after signing")
        (source / "Applications").symlink_to("/Applications")
        image = self.root / f"{source.name}.dmg"
        image.unlink(missing_ok=True)
        subprocess.run(
            ["hdiutil", "create", "-quiet", "-srcfolder", source, "-format", "UDZO", image],
            check=True,
        )
        return image

    def publish(self, version, latest=True, desktop=False, core=None, image=None):
        archive, signature = core or self.core(version)
        assets = {archive.name: archive, signature.name: signature}
        if desktop:
            assets[image_name(version)] = image or self.disk_image(version)
        self.release.publish(version, assets, latest=latest)

    def installed_application(self, version):
        """An application already in place, as an earlier installation left it."""
        self.applications.mkdir(exist_ok=True)
        self.bundle(
            self.applications / "SpaceTerm.app",
            "desktop",
            "SpaceTerm",
            "com.peblon.spaceterm",
            version,
        )

    # Running the installer.

    def environment(self, **extra):
        return {
            "HOME": str(self.home),
            "PATH": f"{self.bin}:/usr/bin:/bin:/usr/sbin:/sbin",
            "SHELL": "/bin/zsh",
            "SERVED": json.dumps(self.release.served),
            "REQUESTS": str(self.requests),
            "CORE_LOG": str(self.core_log),
            "CORE_RELATIVE": CORE_RELATIVE,
            "SPACETERM_INSTALL_DIR": str(self.applications),
            "TMPDIR": str(self.temporary),
            **extra,
        }

    def install(self, *arguments, installer=None, **extra):
        return subprocess.run(
            ["sh", str(installer or self.installer), *arguments],
            env=self.environment(**extra),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=120,
            start_new_session=True,
        )

    def succeed(self, *arguments, **extra):
        result = self.install(*arguments, **extra)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    # Observations.

    def requested(self):
        return self.requests.read_text().splitlines() if self.requests.exists() else []

    def core_calls(self):
        return self.core_log.read_text().splitlines() if self.core_log.exists() else []

    def installations(self):
        directory = self.home / ".local/share/spaceterm/core/installations"
        if not directory.exists():
            return []
        return sorted(
            json.loads((path / "build.json").read_text())["version"] for path in directory.iterdir()
        )

    def register(self, version):
        """Records `version`'s installation as the one the SpaceTerm Core service approves, as
        setup and each activation do."""
        core = self.home / ".local/share/spaceterm/core"
        [digest] = [
            path.name
            for path in (core / "installations").iterdir()
            if json.loads((path / "build.json").read_text())["version"] == version
        ]
        record = {
            "installation": digest,
            "stopped": True,
            "automatic_start": True,
            "installed": True,
            "removing": False,
            "run": None,
        }
        (core / "service.json").write_text(json.dumps(record, separators=(",", ":")))

    def prune(self, version):
        """Removes `version`'s installation, as SpaceTerm Core prunes one it no longer keeps."""
        for path in (self.home / ".local/share/spaceterm/core/installations").iterdir():
            if json.loads((path / "build.json").read_text())["version"] == version:
                shutil.rmtree(path)

    def application_version(self):
        info = self.applications / "SpaceTerm.app/Contents/Info.plist"
        return plistlib.loads(info.read_bytes())["CFBundleShortVersionString"]

    def assert_no_leftovers(self):
        self.assertEqual(list(self.temporary.iterdir()), [])
        if self.applications.exists():
            self.assertEqual(
                sorted(path.name for path in self.applications.iterdir()), ["SpaceTerm.app"]
            )

    # SpaceTerm Core.

    def test_default_installs_only_core_after_verifying_its_code_signature(self):
        result = self.succeed()
        self.assertEqual(
            self.requested(),
            [
                f"{RELEASES}/latest/download/SHA256SUMS",
                f"{RELEASES}/download/v1.2.0/{core_name('1.2.0')}",
                f"{RELEASES}/download/v1.2.0/{core_name('1.2.0')}.sig",
            ],
        )
        [call] = self.core_calls()
        self.assertRegex(call, r"^internal retain .*/runtime/core$")
        self.assertEqual(self.installations(), ["1.2.0"])
        self.assertTrue((self.home / ".local/bin/spaceterm").is_symlink())
        self.assertFalse(self.applications.exists())
        self.assertIn("Run spaceterm setup to start SpaceTerm Core.", result.stdout)
        self.assert_no_leftovers()

    def test_a_release_requires_the_developer_id_before_anything_runs(self):
        installer = render(self.root, current=VALID_KEY, backup=VALID_KEY)
        result = self.install(installer=installer)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "SpaceTerm Core.app is not signed by the SpaceTerm Developer ID", result.stderr
        )
        self.assertEqual(self.core_calls(), [])
        self.assertEqual(self.installations(), [])
        self.assert_no_leftovers()

    def test_a_release_pins_each_bundle_to_its_identifier_and_the_team_id(self):
        """The rendered release passes codesign the complete requirement for each bundle.

        No test certificate chains to Apple, so a recording codesign stands in for the
        signature check and the test reads the requirement the release installer asks for.
        """
        calls = self.root / "codesign.log"
        fake(
            self.bin,
            "codesign",
            f"""#!{sys.executable}
import json, sys
with open({str(calls)!r}, "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
""",
        )
        installer = render(self.root, current=VALID_KEY, backup=VALID_KEY)
        self.succeed("--desktop", installer=installer)

        def requirement(identifier):
            return (
                f'=identifier "{identifier}" and anchor apple generic'
                " and certificate 1[field.1.2.840.113635.100.6.2.6] exists"
                " and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
                f' and certificate leaf[subject.OU] = "{TEAM_ID}"'
            )

        recorded = [json.loads(line) for line in calls.read_text().splitlines()]
        self.assertEqual(
            [(call[:-1], Path(call[-1]).name) for call in recorded],
            [
                (
                    [
                        "--verify",
                        "--deep",
                        "--strict",
                        "--test-requirement=" + requirement("com.peblon.spaceterm.core"),
                    ],
                    "SpaceTerm Core.app",
                ),
                (
                    [
                        "--verify",
                        "--deep",
                        "--strict",
                        "--test-requirement=" + requirement("com.peblon.spaceterm"),
                    ],
                    "SpaceTerm.app",
                ),
            ],
        )
        self.assertEqual(self.installations(), ["1.2.0"])
        self.assertEqual(self.application_version(), "1.2.0")

    def test_unsigned_or_changed_core_bundles_are_refused_before_they_run(self):
        for options in ({"sign": False}, {"tamper": True}):
            with self.subTest(**options):
                self.publish("1.2.0", core=self.core("1.2.0", **options))
                result = self.install()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("SpaceTerm Core.app is not code signed", result.stderr)
                self.assertEqual(self.core_calls(), [])
                self.assertEqual(self.installations(), [])
                self.assert_no_leftovers()

    def test_core_downgrades_are_refused_before_any_archive_downloads(self):
        self.succeed()
        self.requests.unlink()
        self.publish("1.1.0", latest=False)
        result = self.install("--version", "1.1.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SpaceTerm Core 1.2.0 is installed, which is newer than 1.1.0", result.stderr)
        self.assertEqual(self.requested(), [f"{RELEASES}/download/v1.1.0/SHA256SUMS"])

    def test_a_registered_core_below_the_update_floor_is_refused_before_it_runs(self):
        self.publish("1.1.0", latest=False)
        self.succeed("--version", "1.1.0")
        self.register("1.1.0")
        self.publish("1.2.0", core=self.core("1.2.0", floor="1.1.5"))
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "SpaceTerm Core 1.1.0 is older than 1.1.5, the oldest version that updates "
            "directly to SpaceTerm Core 1.2.0; install SpaceTerm Core 1.1.5 first with "
            "--version 1.1.5",
            result.stderr,
        )
        self.assertEqual(self.installations(), ["1.1.0"])

    def test_a_core_without_a_service_record_meets_every_floor(self):
        self.publish("1.1.0", latest=False)
        self.succeed("--version", "1.1.0")
        self.publish("1.2.0", core=self.core("1.2.0", floor="1.1.5"))
        self.succeed()
        self.assertEqual(self.installations(), ["1.1.0", "1.2.0"])

    def test_the_floor_applies_to_the_approved_core_not_one_waiting_beside_it(self):
        for version in ("1.0.0", "1.1.0"):
            self.publish(version, latest=False)
            self.succeed("--version", version)
        self.register("1.0.0")
        self.publish("1.2.0", core=self.core("1.2.0", floor="1.1.0"))
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "SpaceTerm Core 1.0.0 is older than 1.1.0, the oldest version that updates "
            "directly to SpaceTerm Core 1.2.0",
            result.stderr,
        )
        self.assertEqual(self.installations(), ["1.0.0", "1.1.0"])

    def test_a_stale_launcher_does_not_name_the_running_core(self):
        for version in ("1.0.0", "1.1.0"):
            self.publish(version, latest=False)
            self.succeed("--version", version)
        # The activation into 1.1.0 could not replace the launcher, which still names 1.0.0.
        self.register("1.1.0")
        self.publish("1.2.0", core=self.core("1.2.0", floor="1.1.0"))
        self.succeed()
        self.assertEqual(self.installations(), ["1.0.0", "1.1.0", "1.2.0"])

    def test_a_dangling_launcher_does_not_lift_the_floor(self):
        for version in ("1.0.0", "1.1.0"):
            self.publish(version, latest=False)
            self.succeed("--version", version)
        # The launcher still names 1.0.0, which SpaceTerm Core pruned after it moved to 1.1.0.
        self.register("1.1.0")
        self.prune("1.0.0")
        self.publish("1.2.0", latest=False, core=self.core("1.2.0", floor="1.1.0"))
        self.succeed("--version", "1.2.0")
        self.publish("1.3.0", core=self.core("1.3.0", floor="1.2.0"))
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "SpaceTerm Core 1.1.0 is older than 1.2.0, the oldest version that updates "
            "directly to SpaceTerm Core 1.3.0",
            result.stderr,
        )
        self.assertEqual(self.installations(), ["1.1.0", "1.2.0"])

    def test_a_service_record_naming_a_missing_installation_stops_before_downloading(self):
        self.publish("1.1.0", latest=False)
        self.succeed("--version", "1.1.0")
        self.register("1.1.0")
        self.prune("1.1.0")
        self.requests.unlink()
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "the SpaceTerm Core installation the service record approves is missing",
            result.stderr,
        )
        self.assertEqual(self.requested(), [])

    def test_linger_is_refused_on_macos(self):
        result = self.install("--linger")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--linger applies only to Linux", result.stderr)
        self.assertEqual(self.requested(), [])

    def test_profile_edits_add_local_bin_for_zsh_and_bash(self):
        line = 'export PATH="$HOME/.local/bin:$PATH"'
        for shell, profile in (
            ("/bin/zsh", self.home / ".zshrc"),
            ("/bin/bash", self.home / ".bash_profile"),
        ):
            with self.subTest(shell=shell):
                for _ in range(2):
                    self.succeed(SHELL=shell)
                self.assertEqual(
                    profile.read_text(), f"# Added by the SpaceTerm installer\n{line}\n"
                )
        self.assertFalse((self.home / ".bashrc").exists())

    def test_no_modify_path_leaves_profiles_alone(self):
        result = self.succeed("--no-modify-path")
        self.assertFalse((self.home / ".zshrc").exists())
        self.assertIn(f"Add {self.home}/.local/bin to PATH", result.stdout)

    # The desktop app.

    def test_desktop_replaces_the_application_with_the_verified_release(self):
        self.installed_application("1.1.0")
        self.succeed("--desktop")
        self.assertEqual(self.requested()[-1], f"{RELEASES}/download/v1.2.0/{image_name('1.2.0')}")
        self.assertEqual(self.application_version(), "1.2.0")
        self.assertEqual(self.installations(), ["1.2.0"])
        self.assert_no_leftovers()

    def test_unsigned_or_changed_applications_are_not_installed(self):
        self.installed_application("1.1.0")
        for options in ({"sign": False}, {"tamper": True}):
            with self.subTest(**options):
                self.publish("1.2.0", desktop=True, image=self.disk_image("1.2.0", **options))
                result = self.install("--desktop")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("SpaceTerm.app is not code signed", result.stderr)
                self.assertEqual(self.application_version(), "1.1.0")
                self.assert_no_leftovers()

    def test_a_newer_application_is_never_downgraded(self):
        self.installed_application("1.3.0")
        result = self.install("--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SpaceTerm 1.3.0 is installed, which is newer than 1.2.0", result.stderr)
        self.assertEqual(self.requested(), [f"{RELEASES}/latest/download/SHA256SUMS"])
        self.assertEqual(self.application_version(), "1.3.0")

    def test_a_checksum_mismatch_keeps_the_existing_application(self):
        self.installed_application("1.1.0")
        core, signature = self.core("1.2.0")
        image = self.disk_image("1.2.0")
        self.release.publish(
            "1.2.0",
            {core.name: core, signature.name: signature, image_name("1.2.0"): image},
            sums=(
                f"{sha256(core)}  {core.name}\n{sha256(signature)}  {signature.name}\n"
                f"{'f' * 64}  {image_name('1.2.0')}\n"
            ),
        )
        result = self.install("--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match its published checksum", result.stderr)
        self.assertEqual(self.application_version(), "1.1.0")
        self.assert_no_leftovers()

    def test_a_running_application_is_refused_before_downloading(self):
        self.installed_application("1.1.0")
        running = subprocess.Popen(
            [str(self.applications / "SpaceTerm.app/Contents/MacOS/SpaceTerm")]
        )
        self.addCleanup(running.wait)
        self.addCleanup(running.kill)
        result = self.install("--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertEqual(self.requested(), [f"{RELEASES}/latest/download/SHA256SUMS"])
        self.assertEqual(self.application_version(), "1.1.0")

    def test_an_application_started_during_the_download_is_refused(self):
        self.installed_application("1.1.0")
        launched = self.root / "launched.pid"

        def stop():
            if launched.exists():
                os.kill(int(launched.read_text()), signal.SIGKILL)

        self.addCleanup(stop)
        result = self.install(
            "--desktop",
            LAUNCH_DURING_DOWNLOAD=str(
                self.applications / "SpaceTerm.app/Contents/MacOS/SpaceTerm"
            ),
            LAUNCHED=str(launched),
        )
        self.assertTrue(launched.exists())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertEqual(self.application_version(), "1.1.0")
        self.assert_no_leftovers()

    def test_a_failed_replacement_restores_the_existing_application(self):
        self.installed_application("1.1.0")
        # Fail only the rename that moves the staged release into place.
        fake(
            self.bin,
            "mv",
            '#!/bin/sh\ncase "$1" in\n    */.SpaceTerm.install.*/SpaceTerm.app) exit 1 ;;\n'
            'esac\nexec /bin/mv "$@"\n',
        )
        result = self.install("--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not install SpaceTerm.app", result.stderr)
        self.assertEqual(self.application_version(), "1.1.0")
        self.assert_no_leftovers()

    # Uninstall.

    def test_uninstall_desktop_confirms_on_the_terminal_and_removes_the_application(self):
        self.succeed("--desktop")
        # The installer acts on this Mac's SpaceTerm Core whatever the shell selects.
        result, terminal = run_with_tty(
            ["sh", str(self.installer), "--uninstall", "--desktop"],
            self.environment(SPACETERM_MACHINE="paired-machine"),
            "y\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Uninstall SpaceTerm Core?", terminal)
        self.assertEqual(self.core_calls()[1:], ["uninstall", f"uninstall --confirm {TOKEN}"])
        self.assertFalse((self.applications / "SpaceTerm.app").exists())


if __name__ == "__main__":
    unittest.main()
