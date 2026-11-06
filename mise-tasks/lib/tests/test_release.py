"""Exercise release notes, dry-run tags, update trust, feeds, the appcast and the asset list."""

import base64
import json
import os
import plistlib
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, release, release_feed, update_signing
from spaceterm_tasks.release import RELEASES, SPARKLE_NS, notes, verify_appcast

CURRENT = bytes(range(32))
BACKUP = bytes(range(1, 33))
PROTOCOLS = {
    "work": {"major": 2, "minor": 1},
    "service": {"major": 1, "minor": 3},
    "desktop_channel": {"major": 1, "minor": 0},
}


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
        self.git("tag", "-a", "v1.0.0", "-m", "Release")
        self.git("commit", "-qm", "fix(updates)!: preserve active sessions", "--allow-empty")
        self.git("commit", "-qm", "feat: add terminal panes", "--allow-empty")
        self.git("tag", "-a", "v1.1.0", "-m", "Release")
        self.git("commit", "-qm", "feat: future change", "--allow-empty")
        self.git("tag", "-a", "v1.2.0", "-m", "Release")
        self.git("checkout", "-q", "v1.1.0")
        text = notes(self.root)
        self.assertIn("### Fixes", text)
        self.assertIn("**Breaking:** **updates:** Preserve active sessions", text)
        self.assertIn("### Features", text)
        self.assertIn("Add terminal panes", text)
        self.assertNotIn("Initial", text)
        self.assertNotIn("Future change", text)

    def test_first_release_notes_list_no_commits_since_the_previous_application(self):
        self.git("commit", "-qm", "feat: old application feature", "--allow-empty")
        self.git("tag", "-a", "v0.6.8", "-m", "Release")
        self.git("commit", "-qm", "chore: remove legacy compatibility", "--allow-empty")
        self.git("commit", "-qm", "feat: add terminal panes", "--allow-empty")
        self.git("tag", "-a", "v1.0.0", "-m", "Release")
        text = notes(self.root)
        self.assertIn("First release of SpaceTerm.", text)
        self.assertNotIn("Old application feature", text)
        self.assertNotIn("Remove legacy compatibility", text)
        self.assertNotIn("Add terminal panes", text)

    def test_notes_require_a_stable_release_tag_at_head(self):
        self.git("tag", "-a", "v1.0.0", "-m", "Release")
        self.git("commit", "-qm", "feat: unreleased", "--allow-empty")
        self.git("tag", "-a", "v1.1.0-beta.1", "-m", "Prerelease")
        with self.assertRaises(TaskError):
            notes(self.root)

    def test_notes_require_a_release_of_the_new_application_at_head(self):
        self.git("tag", "-a", "v0.6.8", "-m", "Release")
        self.git("commit", "-qm", "feat: unreleased", "--allow-empty")
        self.git("tag", "-a", "v0.7.0", "-m", "Release")
        with self.assertRaises(TaskError):
            notes(self.root)


class DryRunTagTests(unittest.TestCase):
    def test_dry_run_versions_are_newer_than_every_stable_tag_and_start_at_one(self):
        self.assertEqual(release.dry_run_version([]), "1.0.0")
        self.assertEqual(release.dry_run_version(["v0.6.8", "v0.5.0"]), "1.0.0")
        self.assertEqual(release.dry_run_version(["v1.2.3", "v1.10.0", "v0.9.9"]), "1.10.1")
        self.assertEqual(release.dry_run_version(["v2.0.0-beta.1", "v1.0.0", "x"]), "1.0.1")


def write_trust(root, current=CURRENT, backup=BACKUP):
    path = root / update_signing.TRUST
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "current": update_signing.derived_public_key(current),
                "backup": update_signing.derived_public_key(backup),
            }
        )
    )


class TrustTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.message = self.root / "message"
        self.message.write_bytes(b"exact bytes")
        self.signature = self.root / "message.sig"

    def test_the_tracked_release_trust_matches_sparkle_and_requires_signatures(self):
        current, backup = update_signing.release_trust(ROOT)
        self.assertEqual(current, "sNR7I+ZSfwiz/hBiYA0PgrtqqOsJY53yluVe93/tATM=")
        self.assertEqual(backup, "EprlnUGQNGvhtuT8fhIGmVMJpAm1rjGfgkmts2Beesk=")
        plist = plistlib.loads((ROOT / "packaging/macos/spaceterm/Info.plist").read_bytes())
        self.assertEqual(plist["SUPublicEDKey"], current)
        self.assertIs(plist["SURequireSignedFeed"], True)
        self.assertIs(plist["SUVerifyUpdateBeforeExtraction"], True)
        self.assertEqual(plist["SUSignedFeedFailureExpirationInterval"], 0)

    def test_trust_holds_two_distinct_keys_and_nothing_else(self):
        write_trust(self.root)
        keys = update_signing.release_trust(self.root)
        self.assertEqual(keys[0], update_signing.derived_public_key(CURRENT))
        for document in (
            {"current": keys[0], "backup": keys[0]},
            {"current": keys[0]},
            {"current": keys[0], "backup": keys[1], "extra": keys[1]},
            {"current": keys[0], "backup": base64.b64encode(bytes(31)).decode()},
        ):
            with self.subTest(document=document):
                (self.root / update_signing.TRUST).write_text(json.dumps(document))
                with self.assertRaises(TaskError):
                    update_signing.release_trust(self.root)

    def test_a_signature_verifies_under_either_key_and_binds_exact_bytes(self):
        write_trust(self.root)
        keys = update_signing.release_trust(self.root)
        for seed in (CURRENT, BACKUP):
            with self.subTest(seed=seed[:1]):
                update_signing.sign(seed, self.message, self.signature)
                self.assertEqual(self.signature.stat().st_size, 64)
                update_signing.verify(keys, self.message, self.signature)
        update_signing.sign(bytes(32), self.message, self.signature)
        with self.assertRaises(TaskError):
            update_signing.verify(keys, self.message, self.signature)
        update_signing.sign(CURRENT, self.message, self.signature)
        self.message.write_bytes(b"exact bytes!")
        with self.assertRaises(TaskError):
            update_signing.verify(keys, self.message, self.signature)

    def test_only_the_current_key_signs_a_release(self):
        write_trust(self.root)
        current = {update_signing.SIGNING_SECRET: update_signing.encoded_seed(CURRENT)}
        self.assertEqual(update_signing.release_seed(current, self.root), CURRENT)
        for seed in (BACKUP, bytes(32)):
            with self.subTest(seed=seed[:1]):
                environment = {update_signing.SIGNING_SECRET: update_signing.encoded_seed(seed)}
                with self.assertRaises(TaskError):
                    update_signing.release_seed(environment, self.root)
        with self.assertRaises(TaskError):
            update_signing.release_seed({}, self.root)

    def test_a_dry_run_trusts_only_its_generated_key(self):
        seed = update_signing.generated_seed()
        environment = {
            update_signing.DRY_RUN: "1",
            update_signing.SIGNING_SECRET: update_signing.encoded_seed(seed),
        }
        self.assertEqual(
            update_signing.trust(environment, ROOT), (update_signing.derived_public_key(seed),)
        )
        self.assertEqual(update_signing.release_seed(environment, ROOT), seed)

    def test_sparkle_signing_refuses_a_public_key_mismatch(self):
        seed = update_signing.generated_seed()
        environment = {
            update_signing.DRY_RUN: "1",
            update_signing.SIGNING_SECRET: update_signing.encoded_seed(seed),
        }
        public = update_signing.derived_public_key(seed)
        self.assertEqual(
            release.sparkle_signing(environment, {"SUPublicEDKey": public}),
            (["--ed-key-file", "-"], update_signing.encoded_seed(seed).encode()),
        )
        for public in update_signing.release_trust(ROOT):
            with self.subTest(public=public):
                with self.assertRaisesRegex(TaskError, "SUPublicEDKey must be the current"):
                    release.sparkle_signing(environment, {"SUPublicEDKey": public})


class FeedTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.keys = (
            update_signing.derived_public_key(CURRENT),
            update_signing.derived_public_key(BACKUP),
        )

    def signed_archive(self, product, platform, version="1.4.0", seed=CURRENT):
        archive = self.root / release_feed.archive_name(product, version, platform)
        archive.write_bytes(os.urandom(1000))
        update_signing.sign(seed, archive, archive.with_name(f"{archive.name}.sig"))
        return archive

    def write(self, product, platform, archive):
        return release_feed.write(
            CURRENT,
            self.keys,
            self.root,
            product=product,
            platform=platform,
            version="1.4.0",
            published_at=1793750400,
            protocols=PROTOCOLS,
            floor="1.2.0",
            archive=archive,
        )

    def test_each_feed_names_its_signed_archive_and_verifies_before_parsing(self):
        for product, platform in release_feed.FEEDS:
            with self.subTest(product=product, platform=platform):
                archive = self.signed_archive(product, platform)
                feed, signature = self.write(product, platform, archive)
                self.assertEqual(feed.name, f"spaceterm-{product}-{platform}.json")
                self.assertEqual(signature.stat().st_size, 64)
                update_signing.verify(self.keys, feed, signature)
                data = feed.read_bytes()
                self.assertLessEqual(len(data), 4096)
                self.assertFalse(data.endswith(b"\n"))
                document = release_feed.check(data, product, platform)
                self.assertEqual(
                    document,
                    {
                        "schema": 1,
                        "product": product,
                        "platform": platform,
                        "version": "1.4.0",
                        "floor": "1.2.0",
                        "published_at": 1793750400,
                        "protocols": PROTOCOLS,
                        "minimum_system": {"macos-arm64": "26.0", "linux-x86_64": "2.35"}[platform],
                        "notes": f"{RELEASES}/tag/v1.4.0",
                        "archive": {
                            "name": f"spaceterm-{product}-1.4.0-{platform}.tar.gz",
                            "size": 1000,
                            "sha256": update_signing.sha256(archive),
                            "signature": base64.b64encode(
                                archive.with_name(f"{archive.name}.sig").read_bytes()
                            ).decode(),
                        },
                    },
                )

    def test_an_archive_signed_outside_the_trust_gets_no_feed(self):
        archive = self.signed_archive("core", "linux-x86_64", seed=bytes(32))
        with self.assertRaises(TaskError):
            self.write("core", "linux-x86_64", archive)
        self.assertFalse((self.root / "spaceterm-core-linux-x86_64.json").exists())

    def test_the_checker_refuses_every_broken_field_rule(self):
        archive = self.signed_archive("core", "macos-arm64")
        feed, _ = self.write("core", "macos-arm64", archive)
        document = json.loads(feed.read_bytes())

        def compact(value):
            return json.dumps(value, separators=(",", ":")).encode()

        broken = [
            compact({**document, "extra": 1}),
            compact({**document, "schema": 2}),
            compact({**document, "product": "desktop"}),
            compact({**document, "version": "1.04.0"}),
            compact({**document, "published_at": 0}),
            compact({key: value for key, value in document.items() if key != "floor"}),
            compact({**document, "floor": "1.4.1"}),
            compact({**document, "floor": "1.02.0"}),
            compact({**document, "floor": None}),
            compact({**document, "minimum_system": "2.35"}),
            compact({**document, "notes": f"{RELEASES}/tag/v1.4.1"}),
            compact({**document, "protocols": {**PROTOCOLS, "work": {"major": 70000, "minor": 0}}}),
            compact({**document, "archive": {**document["archive"], "name": "other.tar.gz"}}),
            compact({**document, "archive": {**document["archive"], "size": 0}}),
            compact({**document, "archive": {**document["archive"], "sha256": "A" * 64}}),
            json.dumps(document).encode(),
        ]
        for data in broken:
            with self.subTest(data=data[:60]):
                with self.assertRaises(TaskError):
                    release_feed.check(data, "core", "macos-arm64")
        with self.assertRaises(TaskError):
            release_feed.check(feed.read_bytes(), "desktop", "macos-arm64")

    def test_protocols_come_from_the_protocol_version_source(self):
        source = self.root / release_feed.PROTOCOL_SOURCE
        source.parent.mkdir(parents=True)
        source.write_text(
            "pub const WORK: ProtocolVersion = ProtocolVersion { major: 2, minor: 1 };\n"
            "pub const SERVICE: ProtocolVersion = ProtocolVersion { major: 1, minor: 3 };\n"
            "pub const DESKTOP_CHANNEL: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };\n"
            "pub const PREVIOUS_WORK: Option<ProtocolVersion> = None;\n"
        )
        self.assertEqual(release_feed.protocols(self.root), PROTOCOLS)
        source.write_text(source.read_text().replace("SERVICE", "OTHER"))
        with self.assertRaises(TaskError):
            release_feed.protocols(self.root)


def appcast_item(version, floor, length=7):
    signature = base64.b64encode(bytes(64)).decode()
    return (
        "<item>"
        f"<sparkle:version>{version}</sparkle:version>"
        f"<sparkle:shortVersionString>{version}</sparkle:shortVersionString>"
        f"<sparkle:minimumUpdateVersion>{floor}</sparkle:minimumUpdateVersion>"
        "<sparkle:minimumSystemVersion>26.0</sparkle:minimumSystemVersion>"
        f"<sparkle:fullReleaseNotesLink>{RELEASES}/tag/v{version}</sparkle:fullReleaseNotesLink>"
        f'<enclosure url="{RELEASES}/download/v{version}/SpaceTerm-{version}-macos-arm64.dmg" '
        f'length="{length}" sparkle:edSignature="{signature}" /></item>'
    )


def appcast_document(*items):
    return f'<rss xmlns:sparkle="{SPARKLE_NS}"><channel>{"".join(items)}</channel></rss>'


class AppcastTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.feed = self.root / "appcast.xml"

    def archive(self, version):
        archive = self.root / f"SpaceTerm-{version}-macos-arm64.dmg"
        archive.write_bytes(b"fixture")
        return archive

    def test_appcast_must_reference_the_exact_disk_image_version_and_notes(self):
        archive = self.archive("1.0.0")
        signature = base64.b64encode(bytes(64)).decode()
        xml = appcast_document(appcast_item("1.0.0", "1.0.0"))
        self.feed.write_text(xml)
        self.assertEqual(verify_appcast(self.feed, archive, "1.0.0", "1.0.0"), signature)
        for invalid in (
            xml.replace("github.com/", "example.invalid/"),
            xml.replace("<sparkle:version>1.0.0", "<sparkle:version>1.1.0"),
            xml.replace(">26.0<", ">15.0<"),
            xml.replace("tag/v1.0.0", "tag/v0.9.0"),
            xml.replace('length="7"', 'length="8"'),
            xml.replace(signature, base64.b64encode(bytes(63)).decode()),
            xml.replace("<item>", "<item><sparkle:channel>beta</sparkle:channel>"),
            xml.replace("</channel>", "<item /></channel>"),
            xml.replace("<channel><item>", "<channel><item /><item>"),
        ):
            with self.subTest(invalid=invalid):
                self.feed.write_text(invalid)
                with self.assertRaises(ValueError):
                    verify_appcast(self.feed, archive, "1.0.0", "1.0.0")

    def test_the_new_item_names_the_floor_and_each_floor_release_follows_its_step(self):
        archive = self.archive("1.3.0")
        chain = [
            appcast_item("1.3.0", "1.2.0"),
            appcast_item("1.2.0", "1.0.0", length=99),
            appcast_item("1.0.0", "1.0.0", length=42),
        ]
        self.feed.write_text(appcast_document(*chain))
        verify_appcast(self.feed, archive, "1.3.0", "1.2.0")
        broken = [
            # The floor differs from the release's.
            ("1.1.0", chain),
            # The floor release's items are missing, or only some of them follow.
            ("1.2.0", chain[:1]),
            ("1.2.0", chain[:2]),
            # A step names a release other than the previous item's floor.
            ("1.2.0", [chain[0], appcast_item("1.1.0", "1.0.0"), chain[2]]),
            # An item names a floor above its own version.
            ("1.4.0", [appcast_item("1.3.0", "1.4.0"), appcast_item("1.4.0", "1.4.0")]),
            # An item lacks a floor.
            ("1.2.0", [*chain[:2], chain[2].replace("minimumUpdateVersion", "other")]),
            # A step encloses another release's disk image.
            ("1.2.0", [chain[0], chain[1].replace("v1.2.0/", "v1.1.0/"), chain[2]]),
            # The chain goes on past a release that is its own floor.
            ("1.2.0", [*chain, appcast_item("1.0.0", "1.0.0")]),
        ]
        for floor, items in broken:
            with self.subTest(floor=floor, items=len(items)):
                self.feed.write_text(appcast_document(*items))
                with self.assertRaises(ValueError):
                    verify_appcast(self.feed, archive, "1.3.0", floor)


def sparkle_signed(seed, text):
    """`text` with the signature comment Sparkle's sign_update appends to a feed."""
    with tempfile.TemporaryDirectory() as temporary:
        message = Path(temporary) / "feed"
        message.write_bytes(text.encode())
        signature = Path(temporary) / "feed.sig"
        update_signing.sign(seed, message, signature)
        encoded = base64.b64encode(signature.read_bytes()).decode()
    return (
        f"{text}<!-- sparkle-signatures:\nedSignature: {encoded}\nlength: {len(text.encode())}\n-->\n"
    ).encode()


class FloorAppcastTests(unittest.TestCase):
    def setUp(self):
        self.keys = (
            update_signing.derived_public_key(CURRENT),
            update_signing.derived_public_key(BACKUP),
        )
        self.fetched = []
        self.text = appcast_document(appcast_item("1.2.0", "1.2.0")) + "\n"

    def trust(self):
        return self.keys

    def fetch(self, data):
        def fetch(url):
            self.fetched.append(url)
            return data

        return fetch

    def test_a_release_that_is_its_own_floor_fetches_no_appcast(self):
        def refuse():
            raise AssertionError("a release that is its own floor needs no release trust")

        self.assertIsNone(release.floor_appcast("1.2.0", "1.2.0", refuse, self.fetch(b"")))
        self.assertEqual(self.fetched, [])

    def test_the_floor_release_appcast_is_used_only_with_a_trusted_signature(self):
        data = sparkle_signed(BACKUP, self.text)
        self.assertEqual(
            release.floor_appcast("1.2.0", "1.3.0", self.trust, self.fetch(data)), data
        )
        self.assertEqual(self.fetched, [f"{RELEASES}/download/v1.2.0/appcast.xml"])
        for broken in (
            sparkle_signed(bytes(32), self.text),
            data.replace(b"1.2.0</sparkle:version>", b"1.2.1</sparkle:version>"),
            self.text.encode(),
            data + b"<!-- trailing -->",
        ):
            with self.subTest(broken=broken[-80:]):
                with self.assertRaises(TaskError):
                    release.floor_appcast("1.2.0", "1.3.0", self.trust, self.fetch(broken))


class DiskImageUrlTests(unittest.TestCase):
    def test_the_appcast_encloses_the_url_the_desktop_admits(self):
        # src/updates.rs allows_release pins the same URL for the same version.
        self.assertEqual(
            release_feed.download_url("0.1.1", release_feed.dmg_name("0.1.1")),
            "https://github.com/sadiksaifi/SpaceTerm/releases/download/v0.1.1/SpaceTerm-0.1.1-macos-arm64.dmg",
        )


class AssetListTests(unittest.TestCase):
    def test_the_release_holds_every_asset_once_with_one_checksum_file(self):
        names = release_feed.asset_names("1.4.0")
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(
            set(names),
            {
                "SpaceTerm-1.4.0-macos-arm64.dmg",
                "appcast.xml",
                "spaceterm-core-1.4.0-macos-arm64.tar.gz",
                "spaceterm-core-1.4.0-macos-arm64.tar.gz.sig",
                "spaceterm-core-macos-arm64.json",
                "spaceterm-core-macos-arm64.json.sig",
                "spaceterm-core-1.4.0-linux-x86_64.tar.gz",
                "spaceterm-core-1.4.0-linux-x86_64.tar.gz.sig",
                "spaceterm-core-linux-x86_64.json",
                "spaceterm-core-linux-x86_64.json.sig",
                "spaceterm-desktop-1.4.0-linux-x86_64.tar.gz",
                "spaceterm-desktop-1.4.0-linux-x86_64.tar.gz.sig",
                "spaceterm-desktop-linux-x86_64.json",
                "spaceterm-desktop-linux-x86_64.json.sig",
                "install.sh",
                "SHA256SUMS",
            },
        )
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for name in names[:-1]:
                (directory / name).write_bytes(name.encode())
            listed = sorted(directory / name for name in names[:-1])
            (directory / "SHA256SUMS").write_text(release.checksums(listed))
            release.require_assets(directory, "1.4.0")
            (directory / "install.sh").write_bytes(b"changed")
            with self.assertRaisesRegex(TaskError, "checksums"):
                release.require_assets(directory, "1.4.0")
            (directory / "extra").write_bytes(b"")
            with self.assertRaisesRegex(TaskError, "asset list"):
                release.require_assets(directory, "1.4.0")

    def test_a_partial_release_is_only_for_dry_runs(self):
        with self.assertRaisesRegex(TaskError, "only a dry run"):
            release.assemble("v1.4.0", complete=False, environment={})


class InstallerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "install.sh"
        self.output = self.root / "published.sh"

    def pin(self, current, backup):
        text = (ROOT / "packaging/install.sh").read_text()
        for name, key in (("CURRENT", current), ("BACKUP", backup)):
            text = re.sub(
                rf"^readonly UPDATE_KEY_{name}='[^'\n]*'$",
                lambda _: f"readonly UPDATE_KEY_{name}='{key}'",
                text,
                flags=re.M,
            )
        self.source.write_text(text)

    def test_a_release_installer_pins_its_version_and_trust_without_apple_credentials(self):
        keys = update_signing.release_trust(ROOT)
        self.pin(*keys)
        release.installer(self.output, {}, self.source, version="0.10.0")
        text = self.output.read_text()
        self.assertIn("readonly RELEASE_KIND='release'\n", text)
        self.assertIn("readonly DEFAULT_VERSION='0.10.0'\n", text)
        self.assertNotIn("@RELEASE_KIND@", text)
        self.assertNotIn("@DEFAULT_VERSION@", text)
        self.assertTrue(os.access(self.output, os.X_OK))

    def test_the_installer_must_pin_the_tracked_trust(self):
        keys = update_signing.release_trust(ROOT)
        self.pin(keys[1], keys[0])
        with self.assertRaisesRegex(TaskError, "must pin the keys"):
            release.installer(self.output, {}, self.source, version="0.10.0")

    def test_a_dry_run_installer_uses_the_matching_test_trust(self):
        write_trust(self.root)
        self.pin(*update_signing.release_trust(self.root))
        environment = {
            update_signing.DRY_RUN: "1",
            update_signing.SIGNING_SECRET: update_signing.encoded_seed(BACKUP),
        }
        release.installer(self.output, environment, self.source, self.root, version="0.10.0")
        text = self.output.read_text()
        self.assertIn("readonly RELEASE_KIND='dry-run'\n", text)
        key = update_signing.derived_public_key(BACKUP)
        self.assertIn(f"readonly UPDATE_KEY_CURRENT='{key}'\n", text)
        self.assertIn(f"readonly UPDATE_KEY_BACKUP='{key}'\n", text)

    def test_an_invalid_installer_version_is_refused(self):
        self.pin(*update_signing.release_trust(ROOT))
        for version in ("", "v0.10.0", "01.0.0", "0.10.0-beta", "1.2.3'\nexit 0"):
            with self.subTest(version=version), self.assertRaisesRegex(TaskError, "stable release"):
                release.installer(self.output, {}, self.source, version=version)


if __name__ == "__main__":
    unittest.main()
