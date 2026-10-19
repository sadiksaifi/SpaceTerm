"""Desktop packaging keeps signing material out of compiler subprocesses."""

import os
import unittest
from unittest.mock import patch

from spaceterm_tasks import TaskError, package_linux


class PackageCredentialsTests(unittest.TestCase):
    def test_linux_package_does_not_pass_signing_inputs_to_cargo(self):
        environment = {
            "PATH": os.defpath,
            "UPDATE_SIGNING_KEY": "fixture-update-key",
            "MACOS_SIGNING_CERTIFICATE_P12": "fixture-certificate",
            "MACOS_SIGNING_CERTIFICATE_PASSWORD": "fixture-password",
            "SPACETERM_LINUX_PACKAGE_TARGET": "x86_64-unknown-linux-gnu.2.35",
            "SPACETERM_LINUX_PACKAGE_SYSROOT": "/fixture-sysroot",
        }

        def compiler(arguments, **kwargs):
            self.assertTrue(arguments[:2] == ["cargo", "zigbuild"])
            self.assertEqual(
                set(kwargs["env"]) & set(environment),
                {"PATH", "SPACETERM_LINUX_PACKAGE_TARGET", "SPACETERM_LINUX_PACKAGE_SYSROOT"},
                "signing inputs must stay in the packaging process",
            )
            raise TaskError("compiler boundary reached")

        with (
            patch.dict(os.environ, environment, clear=True),
            patch.object(package_linux.sys, "platform", "linux"),
            patch.object(package_linux.platform, "machine", return_value="x86_64"),
            patch("spaceterm_tasks.cargo.subprocess.Popen", side_effect=compiler),
            self.assertRaisesRegex(TaskError, "compiler boundary reached"),
        ):
            package_linux.package()
