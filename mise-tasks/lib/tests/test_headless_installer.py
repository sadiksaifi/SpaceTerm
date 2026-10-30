"""Headless installation refuses unsafe archives before executing an extracted program."""

import io
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import ROOT


class HeadlessInstallerTests(unittest.TestCase):
    def test_local_archive_links_and_parent_paths_are_refused_without_extraction(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home, scratch = root / "home", root / "scratch"
            home.mkdir()
            scratch.mkdir()
            signature = root / "artifact.sig"
            signature.write_bytes(bytes(64))
            archive = root / "candidate.tar.gz"
            for name, kind in (
                ("server/bin/spaceterm", tarfile.SYMTYPE),
                ("server/bin/spaceterm", tarfile.LNKTYPE),
                ("../escaped", tarfile.REGTYPE),
                ("server/../../escaped", tarfile.REGTYPE),
            ):
                with self.subTest(name=name, kind=kind):
                    with tarfile.open(archive, "w:gz") as output:
                        member = tarfile.TarInfo(name)
                        member.type = kind
                        member.mode = 0o755
                        member.linkname = "/bin/sh" if kind != tarfile.REGTYPE else ""
                        member.size = 1 if kind == tarfile.REGTYPE else 0
                        output.addfile(member, io.BytesIO(b"x"))
                    result = subprocess.run(
                        [
                            "sh",
                            ROOT / "packaging/install.sh",
                            "--headless",
                            "--identity",
                            "spaceterm-development",
                            "--archive",
                            archive,
                            "--signature",
                            signature,
                        ],
                        env={**os.environ, "HOME": str(home), "TMPDIR": str(scratch)},
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("CLI archive", result.stderr)
                    self.assertEqual(list(scratch.iterdir()), [])
                    self.assertFalse((root / "escaped").exists())
                    self.assertEqual(list(home.iterdir()), [])

    def test_uninstall_of_an_incompatible_layout_fails_without_naming_a_path(self):
        digest = "a" * 64
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            root = home / ".local/share/spaceterm-development/server"
            # An installation from another layout: its executable is not where this one is.
            old = root / "installations" / digest / "previous/spaceterm"
            old.parent.mkdir(parents=True)
            old.write_text("#!/bin/sh\nexit 0\n")
            old.chmod(0o755)
            (root / "service.json").write_text(json.dumps({"installation": digest}))
            for confirm in ([], ["--confirm", "c1-" + "b" * 64]):
                with self.subTest(confirm=bool(confirm)):
                    result = subprocess.run(
                        [
                            "sh",
                            ROOT / "packaging/install.sh",
                            "--headless",
                            "--identity",
                            "spaceterm-development",
                            "--uninstall",
                            *confirm,
                        ],
                        env={**os.environ, "HOME": str(home)},
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    self.assertEqual(result.returncode, 5, result.stderr)
                    self.assertEqual(
                        result.stderr,
                        "error: incompatible_state: the installed server is from an incompatible"
                        " SpaceTerm. Uninstall it with the installer of that version.\n",
                    )
                    self.assertNotIn(temporary, result.stdout + result.stderr)

    def test_uninstall_leaves_unmanaged_launchers_untouched(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            launcher = home / ".local/bin/spaceterm-development"
            launcher.parent.mkdir(parents=True)
            for link in (False, True):
                with self.subTest(link=link):
                    if link:
                        launcher.symlink_to("/another/tool")
                    else:
                        launcher.write_text("local tool")
                    result = subprocess.run(
                        [
                            "sh",
                            ROOT / "packaging/install.sh",
                            "--headless",
                            "--identity",
                            "spaceterm-development",
                            "--uninstall",
                        ],
                        env={**os.environ, "HOME": str(home)},
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    if link:
                        self.assertEqual(launcher.readlink(), Path("/another/tool"))
                    else:
                        self.assertEqual(launcher.read_text(), "local tool")
                    launcher.unlink()
