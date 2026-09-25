#!/usr/bin/env python3
"""Check the fixed, bounded cursor-response parser without a live terminal."""

import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


SPEC = importlib.util.spec_from_file_location(
    "terminal_resource_workload", Path(__file__).with_name("terminal-resource-workload.py"))
WORKLOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKLOAD)


class CursorReport(unittest.TestCase):
    def test_response_survives_every_read_boundary(self):
        response = b"\x1b[123;456R"
        for boundary in range(1, len(response)):
            parser = WORKLOAD.CursorReportParser()
            with self.subTest(boundary=boundary):
                self.assertIsNone(parser.feed(response[:boundary]))
                self.assertEqual(parser.feed(response[boundary:]), (123, 456))

    def test_noise_and_invalid_reports_cannot_become_coordinates(self):
        for invalid in (b"unrelated input", b"\x1b[0;1R", b"\x1b[1;0R",
                        b"\x1b[65536;1R", b"\x1b[1;999999R", b"\x1b[?1;2R",
                        b"\x1b[1;2;3R", b"\x1b[1;\xffR"):
            parser = WORKLOAD.CursorReportParser()
            with self.subTest(length=len(invalid)):
                self.assertIsNone(parser.feed(invalid))
                self.assertEqual(parser.feed(b"\x1b[12;34R"), (12, 34))

    def test_partial_storage_remains_bounded_and_recovers(self):
        parser = WORKLOAD.CursorReportParser()
        for chunk in (b"\x1b[", b"9" * 4096, b"\x1b[1;", b"\x1b[65535;65535R"):
            result = parser.feed(chunk)
            self.assertLessEqual(len(parser.pending), parser.MAX_BYTES)
        self.assertEqual(result, (65535, 65535))

    def test_smoke_explicitly_skips_without_native_terminal_access(self):
        result = WORKLOAD.acknowledge_consumption(True)
        self.assertEqual(result, {"received": False, "skipped_smoke": True,
                                 "cursor_row": 0, "cursor_column": 0,
                                 "query_bytes": 0, "elapsed_ms": 0.0})

    def test_terminal_modes_restore_after_reply_timeout_and_read_failure(self):
        for outcome in ("reply", "timeout", "read_failure"):
            attributes = ["saved terminal mode"]
            native = SimpleNamespace(tcgetattr=Mock(return_value=attributes),
                                     tcsetattr=Mock(), TCSANOW=0, error=OSError)
            raw = SimpleNamespace(setraw=Mock())
            readiness = [([], [20], []),
                         ([], [], []) if outcome == "timeout" else ([10], [], [])]
            with self.subTest(outcome=outcome), \
                    patch.dict("sys.modules", {"termios": native, "tty": raw}), \
                    patch.object(WORKLOAD.sys, "stdin", SimpleNamespace(fileno=lambda: 10)), \
                    patch.object(WORKLOAD.sys, "stdout", SimpleNamespace(fileno=lambda: 20)), \
                    patch.object(WORKLOAD.os, "isatty", return_value=True), \
                    patch.object(WORKLOAD.os, "get_blocking", return_value=True), \
                    patch.object(WORKLOAD.os, "set_blocking") as blocking, \
                    patch.object(WORKLOAD.select, "select", side_effect=readiness), \
                    patch.object(WORKLOAD.os, "write", return_value=4) as write, \
                    patch.object(WORKLOAD.os, "read", return_value=b"\x1b[2;3R",
                                 side_effect=OSError() if outcome == "read_failure" else None):
                result = WORKLOAD.acknowledge_consumption(False)
            self.assertEqual(result["received"], outcome == "reply")
            self.assertEqual(result["query_bytes"], 4)
            write.assert_called_once_with(20, b"\x1b[6n")
            raw.setraw.assert_called_once_with(10, when=0)
            native.tcsetattr.assert_called_once_with(10, 0, attributes)
            self.assertEqual([call.args for call in blocking.call_args_list],
                             [(20, False), (20, True)])


if __name__ == "__main__":
    unittest.main()
