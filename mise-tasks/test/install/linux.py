#!/usr/bin/env python
# MISE description="Run the release installer against a served fixture release on Linux"
"""Run the release installer against a served fixture release and real archives on Linux."""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

from spaceterm_tasks.package_linux import stage, write_archive
from spaceterm_tasks.release import INSTALLER, RELEASES

APPLICATION_ID = "io.github.sadiksaifi.spaceterm"
# A native executable keeps its own path in /proc/PID/exe, which the installer matches.
FIXTURE = r"""
#include <unistd.h>
int main(void) {
    for (;;) pause();
}
"""


@unittest.skipUnless(sys.platform == "linux", "the Linux installer runs only on Linux")
class InstallTests(unittest.TestCase):
    def setUp(self):
        for tool in ("cc", "tic", "desktop-file-validate"):
            self.assertIsNotNone(shutil.which(tool), f"{tool} is required; run mise doctor project")
        self.directory = tempfile.TemporaryDirectory(prefix="spaceterm-install-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.home = self.root / "home"
        self.home.mkdir()
        # Brackets, quotes, and spaces are literal path characters, not matching or Exec syntax.
        self.install_dir = self.root / 'lib [personal] "quoted" $dollar'
        self.data_home = self.root / "data home"
        self.temporary = self.root / "tmp"
        self.temporary.mkdir()
        self.served = {}
        self.requests = self.root / "requests.log"

        # The network and the host's glibc, architecture, and Vulkan loader are the controlled
        # external boundaries. Unpacking, renaming, and process checks use the real tools.
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.script(
            "curl",
            f"""#!{sys.executable}
import json, os, shutil, sys
arguments = sys.argv[1:]
url = next(argument for argument in arguments if argument.startswith("https://"))
with open(os.environ["REQUESTS"], "a") as log:
    log.write(url + "\\n")
source = json.loads(os.environ["SERVED"]).get(url)
if source is None:
    sys.exit(22)
shutil.copyfile(source, arguments[arguments.index("--output") + 1])
""",
        )
        self.script("getconf", '#!/bin/sh\necho "glibc ${FAKE_GLIBC:-2.35}"\n')
        self.script(
            "uname",
            '#!/bin/sh\nif [ "$1" = -m ]; then echo "${FAKE_MACHINE:-x86_64}"; '
            'else exec /usr/bin/env -i PATH=/usr/bin:/bin uname "$@"; fi\n',
        )
        self.script(
            "ldconfig",
            '#!/bin/sh\n[ -n "${FAKE_NO_VULKAN:-}" ] || '
            "printf '\\tlibvulkan.so.1 (libc6,x86-64) => /usr/lib/libvulkan.so.1\\n'\n",
        )

        self.executable = self.root / "fixture-spaceterm"
        subprocess.run(
            ["cc", "-x", "c", "-", "-o", str(self.executable)],
            input=FIXTURE,
            text=True,
            check=True,
            capture_output=True,
        )
        self.archive = self.release_archive("spaceterm", "0.3.0")
        self.serve("latest/download/SHA256SUMS", self.checksums(self.archive))
        self.serve("download/v0.3.0/SpaceTerm-0.3.0-linux-x86_64.tar.gz", self.archive)
        self.target = self.install_dir / "spaceterm"

    def script(self, name, contents):
        (self.bin / name).write_text(contents)
        (self.bin / name).chmod(0o755)

    def release_archive(self, name, version):
        directory = "spaceterm" if name == "spaceterm" else "spaceterm-preflight"
        tree = self.root / f"tree-{name}-{version}" / directory
        stage(name, self.executable, tree)
        (tree / "share/spaceterm/version").write_text(version)
        archive = self.root / f"{directory}-{version}.tar.gz"
        write_archive(tree, archive)
        if name == "spaceterm":
            release = self.root / f"SpaceTerm-{version}-linux-x86_64.tar.gz"
            archive.rename(release)
            return release
        return archive

    def checksums(self, archive):
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        path = self.root / "SHA256SUMS"
        path.write_text(f"{digest}  {archive.name}\n{'0' * 64}  latest-linux-x86_64.json\n")
        return path

    def serve(self, path, source):
        self.served[f"{RELEASES}/{path}"] = str(source)

    def install(self, *arguments, **extra):
        environment = {
            **os.environ,
            "HOME": str(self.home),
            "XDG_DATA_HOME": str(self.data_home),
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "SERVED": json.dumps(self.served),
            "REQUESTS": str(self.requests),
            "SPACETERM_INSTALL_DIR": str(self.install_dir),
            "TMPDIR": str(self.temporary),
            **extra,
        }
        return subprocess.run(
            ["sh", str(INSTALLER), *arguments],
            env=environment,
            capture_output=True,
            text=True,
            timeout=120,
        )

    def installed(self, previous="0.2.0"):
        """Install an earlier release from a local archive, as the first install would."""
        result = self.install("--archive", str(self.release_archive("spaceterm", previous)))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def installed_version(self):
        return (self.target / "share/spaceterm/version").read_text()

    def requested(self):
        return self.requests.read_text().splitlines() if self.requests.exists() else []

    def assert_no_leftovers(self):
        self.assertEqual(list(self.temporary.iterdir()), [])
        self.assertEqual(sorted(path.name for path in self.install_dir.iterdir()), ["spaceterm"])

    def run_installed(self):
        process = subprocess.Popen([str(self.home / ".local/bin/spaceterm")])
        self.addCleanup(process.wait)
        self.addCleanup(process.kill)
        deadline = time.monotonic() + 5
        while os.readlink(f"/proc/{process.pid}/exe") != str(self.target / "bin/spaceterm"):
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)

    def test_installer_installs_the_latest_checksummed_release_for_the_desktop(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.installed_version(), "0.3.0")
        self.assertEqual(
            self.requested(),
            [
                f"{RELEASES}/latest/download/SHA256SUMS",
                f"{RELEASES}/download/v0.3.0/SpaceTerm-0.3.0-linux-x86_64.tar.gz",
            ],
        )
        launcher = self.home / ".local/bin/spaceterm"
        self.assertEqual(os.readlink(launcher), str(self.target / "bin/spaceterm"))
        icon = self.data_home / f"icons/hicolor/scalable/apps/{APPLICATION_ID}.svg"
        self.assertTrue(icon.is_symlink() and icon.is_file())
        entry = self.data_home / f"applications/{APPLICATION_ID}.desktop"
        subprocess.run(["desktop-file-validate", str(entry)], check=True)
        executable = str(self.target / "bin/spaceterm").replace("$", "\\\\$")
        executable = executable.replace('"', '\\\\"')
        self.assertIn(f'Exec="{executable}"\n', entry.read_text())
        self.assert_no_leftovers()

    def test_installer_replaces_an_earlier_release_and_the_updaters_leftovers(self):
        self.installed()
        (self.install_dir / ".spaceterm.update/tree").mkdir(parents=True)
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.installed_version(), "0.3.0")
        self.assert_no_leftovers()

    def test_installer_keeps_the_existing_installation_when_the_checksum_differs(self):
        self.installed()
        mismatched = self.root / "mismatched-SHA256SUMS"
        mismatched.write_text(f"{'f' * 64}  {self.archive.name}\n")
        self.serve("latest/download/SHA256SUMS", mismatched)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match its published checksum", result.stderr)
        self.assertEqual(self.installed_version(), "0.2.0")
        self.assert_no_leftovers()

    def test_installer_refuses_to_replace_a_running_installation_before_downloading(self):
        self.installed()
        self.run_installed()
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertEqual(self.requested(), [])
        self.assertEqual(self.installed_version(), "0.2.0")

    def test_installer_restores_the_existing_installation_when_replacement_fails(self):
        self.installed()
        # Fail only the rename that moves the unpacked release into place.
        self.script(
            "mv",
            '#!/bin/sh\ncase "$1" in\n    */.spaceterm.install.*/new) exit 1 ;;\nesac\n'
            'exec /bin/mv "$@"\n',
        )
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not install SpaceTerm", result.stderr)
        self.assertEqual(self.installed_version(), "0.2.0")
        self.assert_no_leftovers()

    def test_installer_refuses_unsupported_hosts_before_downloading(self):
        for extra, message in (
            ({"FAKE_MACHINE": "aarch64"}, "requires an x86_64 computer"),
            ({"FAKE_GLIBC": "2.34"}, "requires glibc 2.35 or newer"),
            # musl has no GNU_LIBC_VERSION.
            ({"FAKE_GLIBC": ""}, "requires glibc 2.35 or newer"),
        ):
            with self.subTest(extra=extra):
                if extra.get("FAKE_GLIBC") == "":
                    self.script("getconf", "#!/bin/sh\nexit 1\n")
                result = self.install(**extra)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(self.requested(), [])
                self.assertFalse(self.target.exists())

    def test_installer_warns_but_installs_without_a_vulkan_loader(self):
        result = self.install(FAKE_NO_VULKAN="1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("libvulkan.so.1", result.stderr)
        self.assertEqual(self.installed_version(), "0.3.0")

    def test_preflight_installs_beside_spaceterm_from_a_local_archive(self):
        self.installed()
        preflight = self.release_archive("preflight", "0.0.0")
        result = self.install("--archive", str(preflight))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.requested(), [])
        self.assertEqual(
            sorted(path.name for path in self.install_dir.iterdir()),
            ["spaceterm", "spaceterm-preflight"],
        )
        self.assertEqual(
            os.readlink(self.home / ".local/bin/spaceterm-preflight"),
            str(self.install_dir / "spaceterm-preflight/bin/spaceterm"),
        )
        self.assertTrue(
            (
                self.data_home / "applications/io.github.sadiksaifi.spaceterm-preflight.desktop"
            ).is_file()
        )
        self.assertEqual(self.installed_version(), "0.2.0")

    def test_uninstall_removes_the_installation_and_keeps_user_data(self):
        self.installed()
        settings = self.data_home / "spaceterm/settings.json"
        settings.parent.mkdir(parents=True)
        settings.write_text("{}")
        (self.install_dir / ".spaceterm.update").mkdir()
        result = self.install("--uninstall")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(list(self.install_dir.iterdir()), [])
        self.assertFalse(os.path.lexists(self.home / ".local/bin/spaceterm"))
        self.assertFalse((self.data_home / f"applications/{APPLICATION_ID}.desktop").exists())
        icon = self.data_home / f"icons/hicolor/scalable/apps/{APPLICATION_ID}.svg"
        self.assertFalse(os.path.lexists(icon))
        self.assertEqual(settings.read_text(), "{}")

    def test_uninstall_refuses_while_an_updated_spaceterm_awaits_relaunch(self):
        self.installed()
        self.run_installed()
        # The updater exchanged the running installation into its staging directory.
        staging = self.install_dir / ".spaceterm.update"
        staging.mkdir()
        self.target.rename(staging / "tree")
        shutil.copytree(staging / "tree", self.target, symlinks=True)
        result = self.install("--uninstall")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertTrue((staging / "tree/bin/spaceterm").is_file())

    def test_installer_refuses_a_path_desktop_launchers_cannot_run(self):
        install_dir = self.home / "percent%path"
        archive = self.release_archive("spaceterm", "0.2.0")
        result = self.install("--archive", str(archive), SPACETERM_INSTALL_DIR=str(install_dir))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("desktop launchers cannot run", result.stderr)
        self.assertFalse(install_dir.exists())

    def test_uninstall_refuses_while_spaceterm_runs(self):
        self.installed()
        self.run_installed()
        result = self.install("--uninstall")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertEqual(self.installed_version(), "0.2.0")


if __name__ == "__main__":
    unittest.main()
