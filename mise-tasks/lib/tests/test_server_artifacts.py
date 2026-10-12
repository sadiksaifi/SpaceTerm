"""Server names and metadata never change the shipped desktop namespace."""

import unittest

from spaceterm_tasks import TaskError
from spaceterm_tasks import server_artifacts as artifacts


class ServerArtifactTests(unittest.TestCase):
    def test_server_identifiers_are_stable_and_distinct_for_all_builds(self):
        for build, desktop in (
            ("release", "io.github.sadiksaifi.spaceterm"),
            ("preflight", "io.github.sadiksaifi.spaceterm-preflight"),
            ("development", "io.github.sadiksaifi.spaceterm-development"),
        ):
            self.assertEqual(artifacts.identifier(build), desktop + ".server")
            self.assertTrue(artifacts.bundle_name(build).endswith("Server.app"))

    def test_separate_server_asset_names_preserve_client_names(self):
        from spaceterm_tasks import release

        self.assertEqual(release.linux_archive_name("1.2.3"), "SpaceTerm-1.2.3-linux-x86_64.tar.gz")
        self.assertEqual(release.archive_name("1.2.3"), "SpaceTerm-1.2.3-darwin-arm64.dmg")
        self.assertEqual(
            artifacts.archive_name("1.2.3", "linux-x86_64"),
            "SpaceTerm-Server-1.2.3-linux-x86_64.tar.gz",
        )
        self.assertEqual(artifacts.feed_name("darwin-arm64"), "latest-server-darwin-arm64.json")

    def test_server_entitlements_reject_debug_and_library_validation_exceptions(self):
        artifacts.validate_entitlements({})
        for entitlement in (
            "com.apple.security.get-task-allow",
            "com.apple.security.cs.disable-library-validation",
            "com.apple.security.cs.allow-jit",
            "com.apple.security.cs.allow-unsigned-executable-memory",
            "com.apple.security.cs.allow-dyld-environment-variables",
        ):
            with self.subTest(entitlement=entitlement), self.assertRaises(TaskError):
                artifacts.validate_entitlements({entitlement: True})

    def test_headless_archive_has_no_desktop_or_sparkle_resources(self):
        for name in (
            "Resources/fonts/font.ttf",
            "Sparkle.framework/Sparkle",
            "share/application.desktop",
            "Resources/icon.icns",
            "bin/../server",
            "/bin/server",
            "bin\\server",
        ):
            with self.subTest(name=name), self.assertRaises(TaskError):
                artifacts.validate_headless_name(name)

    def test_duplicate_and_non_json_numbers_are_rejected(self):
        for data in (b'{"a":1,"a":2}', b'{"a":NaN}', b'{"a":Infinity}'):
            with self.assertRaises(TaskError):
                artifacts.strict_json(data, 100)

    def test_malformed_unicode_and_excessive_nesting_are_typed_failures(self):
        for data in (b'{"name":"\\ud800"}', b"[" * 1100 + b"0" + b"]" * 1100):
            with self.subTest(), self.assertRaises(TaskError):
                artifacts.strict_json(data, 4096)
