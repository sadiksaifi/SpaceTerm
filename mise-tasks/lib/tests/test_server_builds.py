"""Both local server builds must describe the same current working tree."""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, cargo
from spaceterm_tasks.server_builds import _source, prepare


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def project(root):
    root.mkdir()
    git(root, "init", "-q")
    git(root, "config", "user.name", "Fixture")
    git(root, "config", "user.email", "fixture@example.invalid")
    (root / "source.rs").write_text("original")
    (root / ".gitignore").write_text("target/\nprivate\n")
    git(root, "add", ".")
    git(root, "commit", "-qm", "initial")


class ServerSourceTests(unittest.TestCase):
    def test_fingerprint_tracks_current_source_and_ignores_private_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "project"
            project(root)
            original = _source(root)[2]
            (root / "private").write_text("ignored")
            self.assertEqual(_source(root)[2], original)
            (root / "source.rs").write_text("dirty")
            dirty = _source(root)[2]
            self.assertNotEqual(dirty, original)
            (root / "untracked.rs").write_text("current")
            untracked = _source(root)[2]
            self.assertNotEqual(untracked, dirty)
            (root / "source.rs").unlink()
            self.assertNotEqual(_source(root)[2], untracked)

    def test_cached_metadata_follows_clean_to_dirty_resource_edits(self):
        # Compile the real server build script in a std-only Cargo fixture. The
        # package boundary stops after reading Cargo metadata, without signing or
        # installing a server or compiling the product dependency graph.
        class MetadataReady(Exception):
            pass

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "project"
            project(root)
            crate = root / "crates/spaceterm-cli"
            (crate / "src").mkdir(parents=True)
            (root / "build").mkdir()
            (root / "assets").mkdir()
            shutil.copyfile(ROOT / "build/identity.rs", root / "build/identity.rs")
            shutil.copyfile(ROOT / "crates/spaceterm-cli/build.rs", crate / "build.rs")
            (root / "Cargo.toml").write_text(
                '[workspace]\nmembers = ["crates/spaceterm-cli"]\nresolver = "3"\n'
            )
            (crate / "Cargo.toml").write_text(
                '[package]\nname = "spaceterm-cli"\nversion = "0.0.0"\nedition = "2024"\n'
                '[[bin]]\nname = "spaceterm-server"\npath = "src/main.rs"\n'
            )
            (crate / "src/main.rs").write_text(
                'fn main() { print!("{}", include_str!(concat!(env!("OUT_DIR"), "/build-info.json"))); }'
            )
            resource = root / "assets/resource.txt"
            resource.write_text("original")
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True)
            git(root, "add", ".")
            git(root, "commit", "-qm", "fixture")
            version = "dev." + git(root, "rev-parse", "HEAD")[:12]
            captured = []

            def build_metadata(identity, *, target):
                environment = dict(os.environ, SPACETERM_PACKAGED="1")
                environment.pop("SPACETERM_RELEASE_TAG", None)
                executable, metadata = cargo.build_executable(
                    "--package",
                    "spaceterm-cli",
                    "--locked",
                    "--offline",
                    binary="spaceterm-server",
                    env=environment,
                    metadata="build-info.json",
                )
                self.assertEqual(
                    json.loads(metadata), json.loads(subprocess.check_output([executable]))
                )
                captured.append(json.loads(metadata)["version"])
                raise MetadataReady

            with (
                patch("spaceterm_tasks.server_builds.ROOT", root),
                patch.object(cargo, "ROOT", root),
                patch("spaceterm_tasks.server_builds.package", build_metadata),
            ):
                for text in ("original", "unstaged resource edit", "original"):
                    resource.write_text(text)
                    with self.assertRaises(MetadataReady):
                        prepare(root / "target/catalog", "spaceterm-preflight")
            self.assertEqual(captured, [version, version + ".dirty", version])

    def test_changed_source_during_build_cannot_publish_a_catalog(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "project"
            project(root)
            destination = Path(temporary) / "catalog"

            def changed_build(identity, *, target):
                (root / "source.rs").write_text("changed during compilation")
                return root / "archive", root / "signature"

            with (
                patch("spaceterm_tasks.server_builds.ROOT", root),
                patch("spaceterm_tasks.server_builds.package", changed_build),
                self.assertRaisesRegex(TaskError, "source changed during server catalog"),
            ):
                prepare(destination, "spaceterm-preflight")
            self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main()
