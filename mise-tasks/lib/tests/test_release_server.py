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
                path.chmod(0o700 if name == "bin/spaceterm-server" else 0o555)
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
        self.assertEqual(feed["schema_version"], 2)
        self.assertEqual(feed["build_commit"], "1" * 40)
        self.assertEqual(
            feed["installation"],
            {"manifest_schema": 2, "registration_schema": 1, "journal_schema": 1},
        )
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

    def test_signed_nonrelease_descriptors_bind_build_commit_and_target(self):
        for build in ("preflight", "development"):
            with self.subTest(build=build):
                document = dict(
                    self.metadata,
                    build_identity=build,
                    native_identifier=artifacts.identifier(build),
                    version="dev." + self.metadata["build_commit"][:12] + ".dirty",
                )
                (self.stage / "installation.json").unlink()
                package_server.write_manifest(self.stage, document)
                self.archive.unlink()
                package_server.write_archive(self.stage, self.archive)
                assets = release_server.sign_assets(
                    self.seed,
                    self.archive,
                    "linux-x86_64",
                    document["version"],
                    int(time.time()),
                    self.assets,
                    build=build,
                )
                self.assertIn(build, assets[0].name)
                self.assertIn(build, assets[1].name)
                accepted = release_server.verify_assets(
                    self.assets,
                    "linux-x86_64",
                    document["version"],
                    self.public,
                    build=build,
                    build_commit=document["build_commit"],
                )
                self.assertEqual(accepted["build_identity"], build)
                with self.assertRaises(TaskError):
                    release_server.verify_assets(
                        self.assets,
                        "linux-x86_64",
                        document["version"],
                        self.public,
                        build=build,
                        build_commit="2" * 40,
                    )
                with self.assertRaises(TaskError):
                    release_server.verify_assets(
                        self.assets,
                        "linux-x86_64",
                        document["version"],
                        self.public,
                        build="release",
                    )

    def test_authenticated_boolean_protocol_and_storage_fields_are_rejected(self):
        import base64

        self.sign()
        feed = self.assets / artifacts.feed_name("linux-x86_64")
        original = json.loads(feed.read_bytes())
        for field, value in (
            ("protocol_majors", [True, 2]),
            ("storage", {"read": [True], "write": True, "migrate_from": [True]}),
        ):
            document = dict(original, **{field: value})
            feed.write_bytes(json.dumps(document).encode())
            signature = self.root / "signature"
            release.sign_ed25519(self.seed, feed, signature)
            (self.assets / (feed.name + ".sig")).write_bytes(
                base64.b64encode(signature.read_bytes())
            )
            with self.subTest(field=field), self.assertRaises(TaskError):
                release_server.verify_assets(self.assets, "linux-x86_64", "1.2.3", self.public)

    def test_feed_interpretation_uses_the_exact_authenticated_snapshot(self):
        import base64
        from unittest.mock import patch

        self.sign()
        feed = self.assets / artifacts.feed_name("linux-x86_64")
        document = json.loads(feed.read_bytes())
        document["published_at"] = int(time.time()) + 100000
        feed.write_bytes(json.dumps(document).encode())
        signature = self.root / "signature"
        release.sign_ed25519(self.seed, feed, signature)
        (self.assets / (feed.name + ".sig")).write_bytes(base64.b64encode(signature.read_bytes()))
        verify = release.verify_ed25519

        def change_input_after_authentication(public, data, signature):
            verify(public, data, signature)
            document["published_at"] = int(time.time())
            feed.write_bytes(json.dumps(document).encode())

        with patch.object(release, "verify_ed25519", side_effect=change_input_after_authentication):
            with self.assertRaises(TaskError):
                release_server.verify_assets(self.assets, "linux-x86_64", "1.2.3", self.public)
