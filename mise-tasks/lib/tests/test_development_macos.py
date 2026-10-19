"""Exercise Development helper staging and refusal before replacing an existing bundle."""

import platform
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.development_macos import stage_bundle


@unittest.skipUnless(
    sys.platform == "darwin" and platform.machine() == "arm64",
    "requires Apple Silicon and macOS icon and signing tools",
)
class DevelopmentBundleTests(unittest.TestCase):
    def test_wrong_helper_build_preserves_the_previous_development_bundle(self):
        with tempfile.TemporaryDirectory(prefix="spaceterm-development-package-") as temporary:
            root = Path(temporary)
            executable = root / "debug/spaceterm"
            executable.parent.mkdir()
            helper = executable.with_name("spaceterm-status-area")

            def compile_executable(path, name):
                subprocess.run(
                    [
                        "xcrun",
                        "clang",
                        "-arch",
                        "arm64",
                        "-mmacosx-version-min=26.0",
                        "-x",
                        "c",
                        "-",
                        "-o",
                        str(path),
                    ],
                    input=(
                        "#include <stdio.h>\n#include <string.h>\n"
                        "int main(int argc, char **argv) {\n"
                        'if (argc != 2 || strcmp(argv[1], "--version")) return 1;\n'
                        f'puts("{name} dev.0123456789ab.dirty"); return 0; }}\n'
                    ),
                    text=True,
                    capture_output=True,
                    check=True,
                )

            compile_executable(executable, "SpaceTerm Development")
            compile_executable(helper, "SpaceTerm Development Server Status")
            bundle = stage_bundle(executable, helper)
            installed = (
                bundle
                / "Contents/Library/LoginItems/SpaceTerm Development Server Status.app"
                / "Contents/MacOS/spaceterm-status-area"
            )
            previous = installed.read_bytes()
            compile_executable(helper, "SpaceTerm Preflight Server Status")
            with self.assertRaisesRegex(TaskError, "status-area executable application identity"):
                stage_bundle(executable, helper)
            self.assertEqual(installed.read_bytes(), previous)
            self.assertFalse(list(bundle.parent.glob(".bundle.*")))
