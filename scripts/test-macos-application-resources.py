#!/usr/bin/env python3
"""Check acceptance and content-free parsing in the native benchmark harness."""

import importlib.util
import json
from pathlib import Path
import unittest
from types import SimpleNamespace
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "application_resources", Path(__file__).with_name("measure-macos-application-resources.py"))
BENCHMARK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCHMARK)


class MemoryProfile(unittest.TestCase):
    def report(self):
        values = {"dirty": 512, "swapped": 0, "clean": 1024,
                  "reclaimable": 0, "wired": 0, "regions": 1}
        return {"unit": "byte", "bytes per unit": 1, "errors": [], "warnings": [], "processes": [
            {"pid": 123, "name": "metadata must not be emitted", "footprint": 8192,
             "page size": 16384,
             "auxiliary": {"phys_footprint": 8190, "phys_footprint_peak": 9000},
             "categories": {"IOSurface": values, "custom allocation name": values,
                            "total": {key: value * 2 for key, value in values.items()}}}]}

    def test_profile_excludes_metadata_and_custom_labels_without_double_counting_totals(self):
        result = BENCHMARK.parse_memory_profile(self.report(), 123)
        self.assertEqual(set(result), {"footprint_bytes", "page_size_bytes", "categories",
                                      "ledger_bytes", "peak_ledger_bytes", "accounting_delta_bytes"})
        self.assertEqual(set(result["categories"]), {"IOSurface", "unlisted_regions"})
        self.assertEqual(result["categories"]["unlisted_regions"]["dirty"], 512)
        self.assertEqual(result["categories"]["unlisted_regions"]["regions"], 1)
        self.assertEqual(result["footprint_bytes"], 8192)
        self.assertEqual(result["accounting_delta_bytes"], 2)

    def test_wrong_process_or_units_or_native_error_rejects_capture(self):
        report = self.report()
        for value, pid in [(report, 124), ({**report, "bytes per unit": 1024}, 123),
                           ({**report, "errors": ["native error must not be emitted"]}, 123),
                           ({**report, "warnings": ["native warning must not be emitted"]}, 123),
                           ({**report, "unit": "page"}, 123),
                           ({**report, "processes": []}, 123), ({}, 123)]:
            with self.subTest(pid=pid):
                with self.assertRaisesRegex(RuntimeError, "^memory profile failed$"):
                    BENCHMARK.parse_memory_profile(value, pid)

    def test_non_numeric_category_rejects_capture(self):
        report = self.report()
        report["processes"][0]["categories"]["IOSurface"]["dirty"] = "invalid"
        with self.assertRaisesRegex(RuntimeError, "^memory profile failed$"):
            BENCHMARK.parse_memory_profile(report, 123)

    def test_invalid_auxiliary_ledger_rejects_capture(self):
        for value in (-1, "invalid", True):
            report = self.report()
            report["processes"][0]["auxiliary"]["phys_footprint"] = value
            with self.subTest(value=value):
                with self.assertRaisesRegex(RuntimeError, "^memory profile failed$"):
                    BENCHMARK.parse_memory_profile(report, 123)


class EqualWorkloads(unittest.TestCase):
    def test_consumption_requires_a_complete_cursor_reply_within_the_grid(self):
        ack = {"received": True, "skipped_smoke": False, "cursor_row": 24,
               "cursor_column": 1, "query_bytes": 4, "elapsed_ms": 0.5}
        summary = {"grid": {"columns": 60, "rows": 24}, "consumption_ack": ack}
        BENCHMARK.validate_consumption(summary)
        for change in ({"received": False}, {"skipped_smoke": True}, {"query_bytes": 3},
                       {"cursor_row": 25}, {"cursor_column": 0}, {"elapsed_ms": -1}):
            with self.subTest(change=change):
                with self.assertRaisesRegex(RuntimeError, "^terminal output consumption was not acknowledged$"):
                    BENCHMARK.validate_consumption({**summary, "consumption_ack": {**ack, **change}})

    def test_equal_output_allows_elapsed_time_variation(self):
        reference = {"mode": "scroll", "updates": 1200, "emitted_lines": 9640,
                     "emitted_bytes": 1050487, "grid": {"columns": 120, "rows": 40},
                     "elapsed_s": 20.001}
        BENCHMARK.validate_workload(reference, {**reference, "elapsed_s": 20.003})

    def test_dropped_output_or_changed_grid_rejects_comparison(self):
        reference = {"mode": "scroll", "updates": 1200, "emitted_lines": 9640,
                     "emitted_bytes": 1050487, "grid": {"columns": 120, "rows": 40}}
        for key, value in [("updates", 1199), ("emitted_lines", 9632),
                           ("emitted_bytes", 1049615),
                           ("grid", {"columns": 119, "rows": 40})]:
            with self.subTest(key=key):
                with self.assertRaisesRegex(RuntimeError, "^workload output changed during comparison$"):
                    BENCHMARK.validate_workload(reference, {**reference, key: value})


class OwnedWindowControl(unittest.TestCase):
    def test_focus_helper_must_not_cover_any_of_the_measured_window(self):
        app = {"X": 306, "Y": 201, "Width": 900, "Height": 580}
        self.assertFalse(BENCHMARK.rectangles_overlap(app, {"X": 8, "Y": 862, "Width": 120, "Height": 88}))
        self.assertTrue(BENCHMARK.rectangles_overlap(app, {"X": 300, "Y": 201, "Width": 120, "Height": 88}))
        self.assertTrue(BENCHMARK.rectangles_overlap(app, app))

    def test_unfocused_visible_requires_visible_geometry_without_focus_or_hide(self):
        observation = {"frontmost": False, "hidden": False, "finished_launching": True,
                       "window_bounds_points": [{"X": 35, "Y": 39, "Width": 900, "Height": 580}]}
        BENCHMARK.validate_window_state("unfocused", observation, observation)
        for changed in ({"frontmost": True}, {"hidden": True}, {"window_bounds_points": []},
                        {"window_bounds_points": [{"X": 35, "Y": 39, "Width": 901, "Height": 580}]}):
            with self.subTest(changed=changed):
                with self.assertRaises(RuntimeError):
                    BENCHMARK.validate_window_state("unfocused", observation, {**observation, **changed})

    def test_floating_targets_only_the_owned_process_window(self):
        listing = [{"app-pid": 456, "window-id": 77}, {"app-pid": 123, "window-id": 88}]
        with patch.object(BENCHMARK.shutil, "which", return_value="aerospace"), \
                patch.object(BENCHMARK.subprocess, "run", side_effect=[
                    SimpleNamespace(returncode=0, stdout=json.dumps(listing)),
                    SimpleNamespace(returncode=0)]) as run:
            BENCHMARK.float_owned_window(123)
        self.assertEqual(run.call_args_list[1].args[0],
                         ["aerospace", "layout", "--window-id", "88", "floating"])

    def test_missing_or_ambiguous_owned_window_never_changes_layout(self):
        for listing in [[], [{"app-pid": 456, "window-id": 77}],
                        [{"app-pid": 123, "window-id": 77},
                         {"app-pid": 123, "window-id": 88}]]:
            with self.subTest(listing=listing), \
                    patch.object(BENCHMARK.shutil, "which", return_value="aerospace"), \
                    patch.object(BENCHMARK.subprocess, "run", return_value=
                                 SimpleNamespace(returncode=0, stdout=json.dumps(listing))) as run:
                with self.assertRaisesRegex(RuntimeError, "^could not float the owned benchmark window$"):
                    BENCHMARK.float_owned_window(123)
                self.assertEqual(run.call_count, 1)


class FrameCounters(unittest.TestCase):
    def sample(self, elapsed, count):
        return {"event": "frame_counters", "elapsed_s": elapsed,
                "counters": dict.fromkeys(BENCHMARK.FRAME_COUNTER_FIELDS, count)}

    def test_only_numeric_counter_deltas_inside_the_capture_are_retained(self):
        output = "unrelated native message\n" + "\n".join(json.dumps(value) for value in [
            {"event": "other", "content": "must not be retained"},
            self.sample(0, 0), self.sample(11, 100), self.sample(19, 1060), self.sample(21, 1300)])
        result = BENCHMARK.collect_frame_counters(output, 10, 20)
        self.assertEqual(result["elapsed_s"], 8)
        self.assertEqual(result["samples"], 2)
        self.assertEqual(result["delta"]["native_vsync_callbacks"], 960)
        self.assertEqual(set(result), {"elapsed_s", "samples", "delta", "cumulative"})

    def test_missing_reset_or_unexpected_counter_data_rejects_capture(self):
        unexpected = self.sample(19, 100)
        unexpected["counters"]["unexpected"] = 1
        for samples in [[], [self.sample(11, 100)],
                        [self.sample(11, 100), self.sample(19, 99)],
                        [self.sample(11, 100), unexpected]]:
            with self.subTest(samples=len(samples)):
                output = "\n".join(json.dumps(value) for value in samples)
                with self.assertRaisesRegex(RuntimeError, "^frame counters unavailable$"):
                    BENCHMARK.collect_frame_counters(output, 10, 20)


class StartupCounters(unittest.TestCase):
    def test_fixed_stages_and_numeric_constructor_values_are_retained(self):
        renderer = dict.fromkeys(BENCHMARK.METAL_STARTUP_FIELDS, 1)
        renderer.update(renderer_index=0, constructor_total_ns=9)
        output = "\n".join(json.dumps(item) for item in [
            {"event": "unrelated", "content": "must not be retained"},
            {"event": "startup_stage", "stage": "app_run_enter", "elapsed_s": 0.001},
            {"event": "metal_startup", **renderer}])
        result = BENCHMARK.collect_startup_counters(output)
        self.assertEqual(result, {"stages_elapsed_s": {"app_run_enter": 0.001},
                                  "metal_renderers": [renderer]})

    def test_unexpected_metadata_or_inconsistent_totals_reject_startup_data(self):
        renderer = dict.fromkeys(BENCHMARK.METAL_STARTUP_FIELDS, 1)
        renderer.update(renderer_index=0, constructor_total_ns=9)
        for item in [
                {"event": "startup_stage", "stage": "unknown", "elapsed_s": 1},
                {"event": "startup_stage", "stage": "app_run_enter", "elapsed_s": -1},
                {"event": "metal_startup", **renderer, "metadata": "unexpected"},
                {"event": "metal_startup", **renderer, "constructor_total_ns": 10}]:
            with self.subTest(event=item["event"]):
                with self.assertRaisesRegex(RuntimeError, "^startup counters invalid$"):
                    BENCHMARK.collect_startup_counters(json.dumps(item))


if __name__ == "__main__":
    unittest.main()
