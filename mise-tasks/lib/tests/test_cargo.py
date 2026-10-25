"""Cargo metadata belongs to the package producing the selected executable."""

import io
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from spaceterm_tasks import TaskError, cargo


class CargoArtifactTests(unittest.TestCase):
    def test_metadata_uses_the_executable_package_build_script(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            executable = root / "server"
            executable.touch(mode=0o755)
            for name in ("selected", "other"):
                (root / name).mkdir()
                (root / name / "build-info.json").write_text(name)
            messages = [
                {"reason": "build-script-executed", "package_id": name, "out_dir": str(root / name)}
                for name in ("selected", "other")
            ] + [
                {
                    "reason": "compiler-artifact",
                    "package_id": "selected",
                    "target": {"name": "spaceterm-server"},
                    "executable": str(executable),
                }
            ]
            process = SimpleNamespace(
                stdout=io.StringIO("\n".join(map(json.dumps, messages))), wait=lambda: 0
            )
            with patch.object(cargo.subprocess, "Popen", return_value=process):
                result = cargo.build_executable(
                    binary="spaceterm-server", metadata="build-info.json"
                )
            self.assertEqual(result, (executable.resolve(), b"selected"))

    def test_metadata_without_selected_package_build_script_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            executable = Path(temporary) / "server"
            executable.touch(mode=0o755)
            message = {
                "reason": "compiler-artifact",
                "package_id": "selected",
                "target": {"name": "spaceterm-server"},
                "executable": str(executable),
            }
            process = SimpleNamespace(stdout=io.StringIO(json.dumps(message)), wait=lambda: 0)
            with patch.object(cargo.subprocess, "Popen", return_value=process):
                with self.assertRaisesRegex(TaskError, "metadata"):
                    cargo.build_executable(binary="spaceterm-server", metadata="build-info.json")
