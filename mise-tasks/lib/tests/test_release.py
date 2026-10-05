"""Exercise release notes, feed verification, and cask updates."""

import base64
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, release
from spaceterm_tasks.release import (
    RELEASES,
    SPARKLE_NS,
    archive_digest,
    notes,
    supersedes,
    verify_feed,
)


class ReleaseNotesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        shutil.copyfile(ROOT / "cliff.toml", self.root / "cliff.toml")
        self.git("init", "-q")
        self.git("commit", "-qm", "Initial", "--allow-empty")

    def git(self, *args):
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", *args],
            cwd=self.root,
            check=True,
            capture_output=True,
        )

    def test_notes_cover_only_the_release_at_head_and_mark_breaking_changes(self):
        self.git("tag", "-a", "v0.1.0", "-m", "Release")
        self.git("commit", "-qm", "fix(updates)!: preserve active sessions", "--allow-empty")
        self.git("commit", "-qm", "feat: add terminal panes", "--allow-empty")
        self.git("tag", "-a", "v0.2.0", "-m", "Release")
        self.git("commit", "-qm", "feat: future change", "--allow-empty")
        self.git("tag", "-a", "v0.3.0", "-m", "Release")
        self.git("checkout", "-q", "v0.2.0")
        text = notes(self.root)
        self.assertIn("### Fixes", text)
        self.assertIn("**Breaking:** **updates:** Preserve active sessions", text)
        self.assertIn("### Features", text)
        self.assertIn("Add terminal panes", text)
        self.assertNotIn("Initial", text)
        self.assertNotIn("Future change", text)

    def test_notes_require_a_stable_release_tag_at_head(self):
        self.git("tag", "-a", "v0.1.0", "-m", "Release")
        self.git("commit", "-qm", "feat: unreleased", "--allow-empty")
        self.git("tag", "-a", "v0.2.0-beta.1", "-m", "Prerelease")
        with self.assertRaises(TaskError):
            notes(self.root)


class FeedTests(unittest.TestCase):
    def test_feed_must_reference_the_exact_release_asset_and_version(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "SpaceTerm-0.1.0-darwin-arm64.dmg"
            archive.write_bytes(b"fixture")
            signature = base64.b64encode(bytes(64)).decode()
            xml = (
                f'<rss xmlns:sparkle="{SPARKLE_NS}"><channel><item>'
                "<sparkle:version>0.1.0</sparkle:version>"
                "<sparkle:shortVersionString>0.1.0</sparkle:shortVersionString>"
                f'<enclosure url="{RELEASES}/download/v0.1.0/{archive.name}" length="7" '
                f'sparkle:edSignature="{signature}" /></item></channel></rss>'
            )
            feed = root / "appcast.xml"
            feed.write_text(xml)
            self.assertEqual(verify_feed(feed, archive, "0.1.0"), signature)
            for invalid in (
                xml.replace("github.com/", "example.invalid/"),
                xml.replace("<sparkle:version>0.1.0", "<sparkle:version>0.2.0"),
                xml.replace('length="7"', 'length="8"'),
                xml.replace(signature, base64.b64encode(bytes(63)).decode()),
                xml.replace("<item>", "<item><sparkle:channel>beta</sparkle:channel>"),
                xml.replace("</channel>", "<item /></channel>"),
            ):
                with self.subTest(invalid=invalid):
                    feed.write_text(invalid)
                    with self.assertRaises(ValueError):
                        verify_feed(feed, archive, "0.1.0")


class CaskTests(unittest.TestCase):
    DIGEST = "a" * 64

    def test_cask_takes_the_checksum_of_the_release_disk_image(self):
        checksums = f"{self.DIGEST}  SpaceTerm-0.3.0-darwin-arm64.dmg\n{'b' * 64}  appcast.xml\n"
        self.assertEqual(archive_digest("0.3.0", checksums), self.DIGEST)
        entry = f"{self.DIGEST}  SpaceTerm-0.3.0-darwin-arm64.dmg\n"
        for invalid in (
            "",
            entry.replace("0.3.0", "0.2.0"),
            entry * 2,
            entry.replace(self.DIGEST, "not-a-digest"),
            entry.replace(self.DIGEST, "A" * 64),
        ):
            with self.subTest(checksums=invalid), self.assertRaises(TaskError):
                archive_digest("0.3.0", invalid)

    def test_an_older_release_never_replaces_a_newer_cask(self):
        self.assertFalse(supersedes("0.3.0", "0.4.0"))
        self.assertFalse(supersedes("0.3.10", "0.10.0"))
        self.assertTrue(supersedes("0.4.0", "0.4.0"))
        self.assertTrue(supersedes("0.10.0", "0.4.0"))


class CaskUpdateTests(unittest.TestCase):
    DIGEST = "ab" * 32

    def update(self, version, sha256):
        with tempfile.TemporaryDirectory() as directory:
            assets = Path(directory)
            (assets / "SHA256SUMS").write_text(f"{self.DIGEST}  {release.archive_name('0.4.0')}\n")
            info = json.dumps({"casks": [{"version": version, "sha256": sha256}]})
            with (
                patch.object(release, "RELEASE_ASSETS", assets),
                patch.object(release, "checked", return_value=info),
                patch.object(release.subprocess, "run") as run,
            ):
                run.return_value.returncode = 0
                release.update_cask("v0.4.0")
        return run

    def test_a_cask_that_already_matches_the_release_is_left_alone(self):
        self.update("0.4.0", self.DIGEST).assert_not_called()
        self.update("0.5.0", "cd" * 32).assert_not_called()

    def test_a_new_release_or_rebuilt_archive_updates_the_cask(self):
        for version, sha256 in (("0.3.0", "cd" * 32), ("0.4.0", "cd" * 32)):
            with self.subTest(version=version):
                command = self.update(version, sha256).call_args.args[0]
                self.assertEqual(
                    command,
                    [
                        "brew",
                        "bump-cask-pr",
                        "--write-only",
                        "--no-audit",
                        "--no-style",
                        "--version",
                        "0.4.0",
                        "--sha256",
                        self.DIGEST,
                        "sadiksaifi/tap/spaceterm",
                    ],
                )


if __name__ == "__main__":
    unittest.main()
