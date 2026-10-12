"""Exercise ordinary Ed25519 server feeds and complete asset authentication."""

import json
import tempfile
import time
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError, package_server, release, release_server
from spaceterm_tasks import server_artifacts as artifacts

from tests.test_package_server import metadata


class ServerReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.stage = self.root / "spaceterm-server"
        self.stage.mkdir()
        self.metadata = metadata()
        for name in artifacts.entry_points("linux-x86_64", "release").values():
            if name:
                path = self.stage / name
                path.parent.mkdir(exist_ok=True, parents=True)
                path.write_bytes(name.encode())
                path.chmod(0o501 if name == "bin/spaceterm-server" else 0o555)
        package_server.write_manifest(self.stage, self.metadata)
        self.archive = self.root / "input.tar.gz"
        package_server.write_archive(self.stage, self.archive)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.seed = b"\x07" * 32
        self.public = release.derived_public_key(self.seed)

    def sign(self):
        return release_server.sign_assets(
            self.seed, self.archive, "linux-x86_64", "1.2.3", int(time.time()), self.assets
        )

    def test_server_feed_and_archive_use_existing_ordinary_ed25519_trust(self):
        assets = self.sign()
        self.assertEqual(len(assets), 3)
        manifest = release_server.verify_assets(self.assets, "linux-x86_64", "1.2.3", self.public)
        self.assertEqual(manifest["storage"], self.metadata["storage"])
        feed = json.loads((self.assets / artifacts.feed_name("linux-x86_64")).read_bytes())
        self.assertEqual(feed["archive"]["name"], "SpaceTerm-Server-1.2.3-linux-x86_64.tar.gz")
        self.assertEqual(feed["installation"]["launcher_abi"], 2)
        self.assertNotIn("url", feed)

    def test_changed_archive_or_feed_cannot_be_published(self):
        assets = self.sign()
        for path in assets:
            original = path.read_bytes()
            path.write_bytes(original + b"altered")
            with self.subTest(asset=path.name), self.assertRaises(TaskError):
                release_server.verify_assets(self.assets, "linux-x86_64", "1.2.3", self.public)
            path.write_bytes(original)

    def test_preflight_never_receives_a_production_feed(self):
        self.metadata["build_identity"] = "preflight"
        self.metadata["native_identifier"] = artifacts.identifier("preflight")
        (self.stage / "installation.json").unlink()
        package_server.write_manifest(self.stage, self.metadata)
        self.archive.unlink()
        package_server.write_archive(self.stage, self.archive)
        with self.assertRaises(TaskError):
            self.sign()

    def test_authenticated_malformed_archive_signature_has_a_typed_failure(self):
        import base64

        self.sign()
        feed = self.assets / artifacts.feed_name("linux-x86_64")
        document = json.loads(feed.read_bytes())
        document["archive"]["signature"] = None
        feed.write_bytes(json.dumps(document).encode())
        signature = self.root / "signature"
        release.sign_ed25519(self.seed, feed, signature)
        (self.assets / (feed.name + ".sig")).write_bytes(
            base64.b64encode(signature.read_bytes()) + b"\n"
        )
        with self.assertRaises(TaskError):
            release_server.verify_assets(self.assets, "linux-x86_64", "1.2.3", self.public)
