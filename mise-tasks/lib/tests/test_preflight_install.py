"""Install the locally verified Preflight archive without the release installer."""

import os
import runpy
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT
from spaceterm_tasks.package_linux import application_id


class PreflightInstallTests(unittest.TestCase):
    def test_linux_task_installs_its_local_archive_and_registers_preflight(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "home space"
            data = root / "data"
            tree = root / "spaceterm-preflight"
            executable = tree / "bin/spaceterm-desktop"
            executable.parent.mkdir(parents=True)
            executable.write_text("#!/bin/sh\nexit 0\n")
            executable.chmod(0o755)
            identifier = application_id("preflight")
            entry = tree / "share/applications" / f"{identifier}.desktop"
            entry.parent.mkdir(parents=True)
            entry.write_text((ROOT / "packaging/linux/preflight" / entry.name).read_text())
            icon = tree / "share/icons/hicolor/scalable/apps" / f"{identifier}.svg"
            icon.parent.mkdir(parents=True)
            icon.write_text("<svg/>\n")
            archive = root / "preflight.tar.gz"
            with tarfile.open(archive, "w:gz") as output:
                output.add(tree, arcname=tree.name)
            release = home / ".local/lib/spaceterm/untouched"
            release.parent.mkdir(parents=True)
            release.write_text("release")
            target = home / ".local/lib/spaceterm-preflight"
            target.mkdir()
            (target / "old").write_text("old")
            with (
                patch.dict(os.environ, {"HOME": str(home), "XDG_DATA_HOME": str(data)}),
                patch("spaceterm_tasks.package_linux.package") as package,
                patch("spaceterm_tasks.package_linux.archive_path", return_value=archive),
            ):
                task = runpy.run_path(str(ROOT / "mise-tasks/preflight/install/linux.py"))
                task["install"]()
            package.assert_called_once_with()
            self.assertTrue(os.access(target / "bin/spaceterm-desktop", os.X_OK))
            self.assertFalse((target / "old").exists())
            self.assertEqual(release.read_text(), "release")
            self.assertEqual(
                (home / ".local/bin/spaceterm-preflight-desktop").resolve(),
                (target / "bin/spaceterm-desktop").resolve(),
            )
            registered = data / "applications" / entry.name
            self.assertIn(f'Exec="{target}/bin/spaceterm-desktop" %u\n', registered.read_text())
            # The entry names the installation's own icon instead of linking it into the theme.
            self.assertIn(f"Icon={target / icon.relative_to(tree)}\n", registered.read_text())
            self.assertFalse((data / "icons").exists())
