import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks.ghostty import run_native_tests


class NativeTests(unittest.TestCase):
    def test_relative_source_with_spaces_owns_fixture_lookup_and_preserves_exit_status(self):
        with tempfile.TemporaryDirectory(prefix="native tests ") as directory:
            root = Path(directory)
            source = root / "prepared source"
            source.mkdir()
            (source / "build.zig").write_text("")
            (source / "native-fixture.txt").write_text("golden fixture")
            tools = root / "tools"
            tools.mkdir()
            zig = tools / "zig"
            zig.write_text(
                f"#!{sys.executable}\n"
                "from pathlib import Path\n"
                "import sys\n"
                "fixture = Path('native-fixture.txt').read_text()\n"
                "Path('observed.txt').write_text(fixture)\n"
                "sys.exit(7)\n"
            )
            zig.chmod(0o755)
            with patch.dict(os.environ, {"PATH": str(tools) + os.pathsep + os.environ["PATH"]}):
                status = run_native_tests(Path(os.path.relpath(source)))
            self.assertEqual(status, 7)
            self.assertEqual((source / "observed.txt").read_text(), "golden fixture")

    def test_unprepared_source_is_rejected_before_tool_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            not_directory = root / "file"
            not_directory.write_text("")
            for source in (root / "missing", root, not_directory):
                with self.subTest(source=source), patch("subprocess.Popen") as launch:
                    with self.assertRaises(TaskError):
                        run_native_tests(source)
                    launch.assert_not_called()
