#!/usr/bin/env python3
"""Compare owned source builds with fresh, isolated macOS terminal workloads."""

import argparse
import ctypes
import hashlib
import json
import math
import os
from pathlib import Path
import plistlib
import shutil
import signal
import select
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parent.parent
WORKLOAD = ROOT / "scripts/terminal-resource-workload.py"
SAMPLER = ROOT / "scripts/measure-macos-terminal-resources.py"
PLIST = ROOT / "packaging/macos/Info.plist"
BENCHMARK_FAILURES = frozenset({
    "cannot enumerate on-screen windows",
    "owned application is not running",
    "could not hide the owned application",
    "could not focus the owned application",
    "owned application was not frontmost",
    "owned application window count was not one",
    "owned application was not fully launched",
    "hidden application state was not verified",
    "application window geometry changed during comparison",
    "application exited before the workload reported readiness",
    "timed out waiting for the owned workload",
    "resource sampler failed",
    "resource sampler returned no intervals",
    "history fixture did not report the expected output",
    "producer ran the wrong fixture",
    "native window observation failed",
    "memory profile failed",
    "workload output changed during comparison",
    "frame counters unavailable",
    "startup counters invalid",
    "could not float the owned benchmark window",
    "unfocused visible application state was not verified",
    "owned focus helper was not ready",
    "terminal output consumption was not acknowledged",
})


def native_window_state(pid):
    """Read only the owned window's bounds and the frontmost process identifier."""
    quartz = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    foundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    ctypes.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit")
    objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
    quartz.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    quartz.CGWindowListCopyWindowInfo.restype = ctypes.c_void_p
    foundation.CFArrayGetCount.argtypes = [ctypes.c_void_p]
    foundation.CFArrayGetCount.restype = ctypes.c_long
    foundation.CFArrayGetValueAtIndex.argtypes = [ctypes.c_void_p, ctypes.c_long]
    foundation.CFArrayGetValueAtIndex.restype = ctypes.c_void_p
    foundation.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    foundation.CFDictionaryGetValue.restype = ctypes.c_void_p
    foundation.CFNumberGetValue.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p]
    foundation.CFNumberGetValue.restype = ctypes.c_bool
    foundation.CFStringCreateWithCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32]
    foundation.CFStringCreateWithCString.restype = ctypes.c_void_p
    foundation.CFRelease.argtypes = [ctypes.c_void_p]
    objc.objc_getClass.argtypes = [ctypes.c_char_p]
    objc.objc_getClass.restype = ctypes.c_void_p
    objc.sel_registerName.argtypes = [ctypes.c_char_p]
    objc.sel_registerName.restype = ctypes.c_void_p
    message_object = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p)(("objc_msgSend", objc))
    message_pid = ctypes.CFUNCTYPE(ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p)(("objc_msgSend", objc))
    workspace = message_object(objc.objc_getClass(b"NSWorkspace"), objc.sel_registerName(b"sharedWorkspace"))
    active = message_object(workspace, objc.sel_registerName(b"frontmostApplication"))
    frontmost_pid = message_pid(active, objc.sel_registerName(b"processIdentifier")) if active else None
    running_application = ctypes.CFUNCTYPE(
        ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int
    )(("objc_msgSend", objc))
    owned = running_application(
        objc.objc_getClass(b"NSRunningApplication"),
        objc.sel_registerName(b"runningApplicationWithProcessIdentifier:"), pid,
    )
    message_bool = ctypes.CFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)(("objc_msgSend", objc))
    hidden = message_bool(owned, objc.sel_registerName(b"isHidden")) if owned else None
    finished_launching = message_bool(owned, objc.sel_registerName(b"isFinishedLaunching")) if owned else None

    def number(dictionary, key, kind):
        value = foundation.CFDictionaryGetValue(dictionary, key)
        if not value:
            return None
        result = ctypes.c_double() if kind == 13 else ctypes.c_longlong()
        return result.value if foundation.CFNumberGetValue(value, kind, ctypes.byref(result)) else None

    pid_key = ctypes.c_void_p.in_dll(quartz, "kCGWindowOwnerPID").value
    layer_key = ctypes.c_void_p.in_dll(quartz, "kCGWindowLayer").value
    bounds_key = ctypes.c_void_p.in_dll(quartz, "kCGWindowBounds").value
    coordinate_keys = {
        name: foundation.CFStringCreateWithCString(None, name.encode("ascii"), 0x08000100)
        for name in ("X", "Y", "Width", "Height")
    }
    windows = quartz.CGWindowListCopyWindowInfo(1 | 16, 0)
    if not windows:
        raise RuntimeError("cannot enumerate on-screen windows")
    bounds = []
    try:
        for index in range(foundation.CFArrayGetCount(windows)):
            window = foundation.CFArrayGetValueAtIndex(windows, index)
            if number(window, pid_key, 4) != pid or number(window, layer_key, 4) != 0:
                continue
            rect = foundation.CFDictionaryGetValue(window, bounds_key)
            if not rect:
                continue
            values = {name: number(rect, key, 13) for name, key in coordinate_keys.items()}
            if all(value is not None for value in values.values()):
                bounds.append(values)
    finally:
        foundation.CFRelease(windows)
        for key in coordinate_keys.values():
            foundation.CFRelease(key)
    return {"frontmost": frontmost_pid == pid, "hidden": hidden,
            "finished_launching": finished_launching, "window_bounds_points": bounds}


def observe_window_state(pid):
    result = subprocess.run(
        [sys.executable, __file__, "--internal-state", str(pid)],
        capture_output=True, text=True, check=False, timeout=5,
    )
    if result.returncode != 0:
        raise RuntimeError("native window observation failed")
    return json.loads(result.stdout)


def native_set_owned_application_state(pid, state):
    ctypes.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit")
    objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
    objc.objc_getClass.argtypes = [ctypes.c_char_p]
    objc.objc_getClass.restype = ctypes.c_void_p
    objc.sel_registerName.argtypes = [ctypes.c_char_p]
    objc.sel_registerName.restype = ctypes.c_void_p
    running_application = ctypes.CFUNCTYPE(
        ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int
    )(("objc_msgSend", objc))
    owned = running_application(
        objc.objc_getClass(b"NSRunningApplication"),
        objc.sel_registerName(b"runningApplicationWithProcessIdentifier:"), pid,
    )
    if not owned:
        raise RuntimeError("owned application is not running")
    if state == "hidden":
        hide = ctypes.CFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)(("objc_msgSend", objc))
        if not hide(owned, objc.sel_registerName(b"hide")):
            # Some macOS versions reject the cross-process hide request. Deliver
            # the existing Hide shortcut only to the owned, already focused app.
            quartz = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
            foundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
            quartz.CGPreflightPostEventAccess.restype = ctypes.c_bool
            if not quartz.CGPreflightPostEventAccess():
                raise RuntimeError("could not hide the owned application")
            quartz.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
            quartz.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
            quartz.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
            quartz.CGEventPostToPid.argtypes = [ctypes.c_int, ctypes.c_void_p]
            foundation.CFRelease.argtypes = [ctypes.c_void_p]
            for pressed in (True, False):
                event = quartz.CGEventCreateKeyboardEvent(None, 4, pressed)  # kVK_ANSI_H
                if not event:
                    raise RuntimeError("could not hide the owned application")
                try:
                    quartz.CGEventSetFlags(event, 1 << 20)  # kCGEventFlagMaskCommand
                    quartz.CGEventPostToPid(pid, event)
                finally:
                    foundation.CFRelease(event)
    else:
        activate = ctypes.CFUNCTYPE(
            ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong
        )(("objc_msgSend", objc))
        if not activate(owned, objc.sel_registerName(b"activateWithOptions:"), 2):
            raise RuntimeError("could not focus the owned application")


def set_owned_application_state(pid, state):
    result = subprocess.run(
        [sys.executable, __file__, "--internal-action", state, str(pid)],
        capture_output=True, text=True, check=False, timeout=5,
    )
    if result.returncode != 0:
        action = "hide" if state == "hidden" else "focus"
        raise RuntimeError(f"could not {action} the owned application")


def validate_window_state(state, before, after):
    for observation in (before, after):
        bounds = observation["window_bounds_points"]
        if not observation["finished_launching"]:
            raise RuntimeError("owned application was not fully launched")
        if state == "focused" and not observation["frontmost"]:
            raise RuntimeError("owned application was not frontmost")
        if state in ("focused", "unfocused") and len(bounds) != 1:
            raise RuntimeError("owned application window count was not one")
        if state == "unfocused" and (observation["frontmost"] or observation["hidden"]):
            raise RuntimeError("unfocused visible application state was not verified")
        if state == "hidden" and (observation["frontmost"] or bounds or not observation["hidden"]):
            raise RuntimeError("hidden application state was not verified")
    if state in ("focused", "unfocused") and before["window_bounds_points"] != after["window_bounds_points"]:
        raise RuntimeError("application window geometry changed during comparison")


def float_owned_window(pid):
    """Exclude only the owned window from AeroSpace's changing tiled geometry."""
    executable = shutil.which("aerospace")
    if executable is None:
        raise RuntimeError("could not float the owned benchmark window")
    listing = subprocess.run(
        [executable, "list-windows", "--all", "--format", "%{window-id} %{app-pid}", "--json"],
        capture_output=True, text=True, timeout=5, check=False)
    try:
        windows = [item["window-id"] for item in json.loads(listing.stdout)
                   if item["app-pid"] == pid]
    except (ValueError, KeyError, TypeError) as error:
        raise RuntimeError("could not float the owned benchmark window") from error
    if listing.returncode != 0 or len(windows) != 1 or type(windows[0]) is not int:
        print(json.dumps({"event": "window_control_failure", "phase": "listing",
                          "status": listing.returncode, "owned_windows": len(windows)}), flush=True)
        raise RuntimeError("could not float the owned benchmark window")
    result = subprocess.run([executable, "layout", "--window-id", str(windows[0]), "floating"],
                            capture_output=True, text=True, timeout=5, check=False)
    if result.returncode != 0:
        print(json.dumps({"event": "window_control_failure", "phase": "layout",
                          "status": result.returncode}), flush=True)
        raise RuntimeError("could not float the owned benchmark window")


def start_focus_owner(application_state):
    helper = subprocess.Popen([sys.executable, str(ROOT / "scripts/macos-benchmark-focus-owner.py")],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        readable, _, _ = select.select([helper.stdout], [], [], 12)
        if not readable or helper.stdout.readline() != b"1\n":
            raise RuntimeError("owned focus helper was not ready")
        validate_focus_owner(helper, application_state)
        return helper
    except BaseException:
        stop_focus_owner(helper)
        raise


def validate_focus_owner(helper, application_state):
    if helper.poll() is not None:
        raise RuntimeError("owned focus helper was not ready")
    observed = observe_window_state(helper.pid)
    helper_bounds = observed["window_bounds_points"]
    app_bounds = application_state["window_bounds_points"]
    if (not observed["frontmost"] or not observed["finished_launching"]
            or len(helper_bounds) != 1 or len(app_bounds) != 1
            or rectangles_overlap(helper_bounds[0], app_bounds[0])):
        raise RuntimeError("owned focus helper was not ready")


def rectangles_overlap(first, second):
    return (first["X"] < second["X"] + second["Width"]
            and second["X"] < first["X"] + first["Width"]
            and first["Y"] < second["Y"] + second["Height"]
            and second["Y"] < first["Y"] + first["Height"])


def stop_focus_owner(helper):
    helper.stdin.close()
    try:
        helper.wait(timeout=5)
    except subprocess.TimeoutExpired:
        helper.kill()
        helper.wait(timeout=5)
    helper.stdout.close()


def wait_for_file(path, app, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return json.loads(path.read_text(encoding="utf-8"))
        if app.poll() is not None:
            raise RuntimeError("application exited before the workload reported readiness")
        time.sleep(0.05)
    raise RuntimeError("timed out waiting for the owned workload")


def validate_workload(reference, summary):
    for key in ("mode", "updates", "emitted_lines", "emitted_bytes", "grid"):
        if summary.get(key) != reference.get(key):
            raise RuntimeError("workload output changed during comparison")


def validate_consumption(summary):
    ack = summary.get("consumption_ack")
    grid = summary.get("grid")
    fields = {"received", "skipped_smoke", "cursor_row", "cursor_column", "query_bytes", "elapsed_ms"}
    if (not isinstance(ack, dict) or set(ack) != fields or not isinstance(grid, dict)
            or any(type(grid.get(key)) is not int for key in ("rows", "columns"))
            or ack["received"] is not True or ack["skipped_smoke"] is not False
            or type(ack["query_bytes"]) is not int or ack["query_bytes"] != 4
            or type(ack["elapsed_ms"]) not in (int, float)
            or not math.isfinite(ack["elapsed_ms"]) or ack["elapsed_ms"] < 0
            or any(type(ack[key]) is not int for key in ("cursor_row", "cursor_column"))
            or not 1 <= ack["cursor_row"] <= grid["rows"]
            or not 1 <= ack["cursor_column"] <= grid["columns"]):
        raise RuntimeError("terminal output consumption was not acknowledged")


def sample(pid, duration, interval):
    return subprocess.Popen(
        [sys.executable, str(SAMPLER), str(pid), "--duration", str(duration), "--interval", str(interval)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )


def collect(process, timeout):
    stdout, _ = process.communicate(timeout=timeout)
    if process.returncode != 0:
        raise RuntimeError("resource sampler failed")
    rows = [json.loads(line) for line in stdout.splitlines()]
    if not rows:
        raise RuntimeError("resource sampler returned no intervals")
    elapsed = sum(row["interval_s"] for row in rows)
    return {
        "cpu_percent": sum(row["cpu_percent"] * row["interval_s"] for row in rows) / elapsed,
        "interrupt_wakeups_s": sum(row["interrupt_wakeups_s"] * row["interval_s"] for row in rows) / elapsed,
        "package_wakeups_s": sum(row["package_wakeups_s"] * row["interval_s"] for row in rows) / elapsed,
        "footprint_mib": sum(row["footprint_mib"] * row["interval_s"] for row in rows) / elapsed,
        "resident_mib": sum(row["resident_mib"] * row["interval_s"] for row in rows) / elapsed,
        "samples": len(rows),
        "elapsed_s": elapsed,
    }


def stage_binary(source, destination):
    binary = destination / "SpaceTerm.app/Contents/MacOS/SpaceTerm"
    binary.parent.mkdir(parents=True)
    with PLIST.open("rb") as source_plist:
        info = plistlib.load(source_plist)
    info["CFBundleIdentifier"] = f"io.github.sadiksaifi.spaceterm-benchmark-{destination.name}"
    with (binary.parent.parent / "Info.plist").open("wb") as staged_plist:
        plistlib.dump(info, staged_plist)
    shutil.copy2(source, binary)
    subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(binary.parent.parent.parent)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return binary


def memory_profile(pid):
    # Inspect only the owned process, outside its timed output interval. Native
    # metadata stays in a temporary directory and is never emitted by the harness.
    with tempfile.TemporaryDirectory(prefix="spaceterm-memory-profile-") as temporary:
        destination = Path(temporary) / "profile.json"
        result = subprocess.run(["footprint", "-p", str(pid), "-j", str(destination), "-f", "bytes",
                                 "--noDrainDeferredReclaim"],
                                capture_output=True, text=True, check=False, timeout=15)
        if result.returncode != 0 or not destination.is_file():
            raise RuntimeError("memory profile failed")
        try:
            report = json.loads(destination.read_text())
        except (ValueError, OSError) as error:
            raise RuntimeError("memory profile failed") from error
    return parse_memory_profile(report, pid)


def parse_memory_profile(report, pid):
    allowed = {"IOAccelerator", "IOSurface", "IOKit", "VM_ALLOCATE", "Untagged",
               "Malloc Guard Page", "Malloc Metadata", "Malloc Metadata metadata",
               "Malloc Large", "Malloc Large Reusable", "Malloc Large Reused",
               "Malloc Small", "Malloc Small (empty)", "Malloc Tiny", "Malloc Tiny (empty)",
               "Malloc Nano", "Malloc Medium", "Malloc Medium (empty)", "Stack", "STACK GUARD",
               "mapped file", "CG image", "CoreAnimation", "CoreGraphics", "page table",
               "__DATA", "__DATA_CONST", "__DATA_DIRTY", "__AUTH", "__AUTH_CONST",
               "__LINKEDIT", "__TEXT"}
    allowed.update(f"{name} ({ledger})"
                   for name in ("IOAccelerator", "IOSurface", "IOKit",
                                "Owned physical footprint (unmapped)")
                   for ledger in ("graphics", "media", "network", "neural"))
    allowed.update(("Deferred Reclaim", "GPU Carveout (reserved)"))
    fields = ("dirty", "swapped", "clean", "reclaimable", "wired", "regions")
    try:
        processes = report["processes"]
        if (report["unit"] != "byte" or type(report["bytes per unit"]) is not int
                or report["bytes per unit"] != 1 or report.get("errors")
                or report.get("warnings") or len(processes) != 1):
            raise ValueError()
        process = processes[0]
        if process["pid"] != pid:
            raise ValueError()
        footprint = process["footprint"]
        page_size = process["page size"]
        ledger = process["auxiliary"]["phys_footprint"]
        peak = process["auxiliary"]["phys_footprint_peak"]
        if type(footprint) is not int or footprint < 0 or type(page_size) is not int or page_size <= 0:
            raise ValueError()
        if any(type(value) is not int or value < 0 for value in (ledger, peak)):
            raise ValueError()
        categories = {}
        unlisted = dict.fromkeys(fields, 0)
        for name, values in process["categories"].items():
            bounded = {field: values[field] for field in fields}
            if any(type(value) is not int or value < 0 for value in bounded.values()):
                raise ValueError()
            if name == "total":
                continue
            if name in allowed:
                categories[name] = bounded
            else:
                for field, value in bounded.items():
                    unlisted[field] += value
        categories["unlisted_regions"] = unlisted
    except (KeyError, TypeError, ValueError, AttributeError) as error:
        raise RuntimeError("memory profile failed") from error
    return {"footprint_bytes": footprint, "page_size_bytes": page_size, "categories": categories,
            "ledger_bytes": ledger, "peak_ledger_bytes": peak,
            "accounting_delta_bytes": footprint - ledger}


FRAME_COUNTER_FIELDS = set("""native_vsync_callbacks window_vsync_callbacks native_sync_frames
logical_frames clean_frames scene_draws scene_draws_with_paths path_texture_allocation_sets
scene_presents next_frame_callbacks frames_with_callbacks
frames_with_dirty_scene frames_with_pending_presentation frames_with_input_grace
frames_requiring_presentation frames_forcing_render native_links_created native_links_started
native_links_stopped window_sources_created window_sources_released window_sources_subscribed
window_sources_unsubscribed native_start_failures""".split())

METAL_STARTUP_FIELDS = set("""renderer_index constructor_total_ns device_ns layer_ns library_ns
vertex_buffer_ns pipelines_ns command_queue_ns backdrop_ns atlas_ns video_cache_ns""".split())
STARTUP_STAGES = frozenset("""app_run_enter app_run_callback_enter settings_load_begin settings_loaded
appearance_installed initial_window_open_enter initial_window_opened""".split())


def collect_startup_counters(output):
    stages = {}
    renderers = []
    for line in output.splitlines():
        try:
            sample = json.loads(line)
        except ValueError:
            continue
        if not isinstance(sample, dict):
            continue
        if sample.get("event") == "startup_stage":
            stage = sample.get("stage")
            elapsed = sample.get("elapsed_s")
            if (stage not in STARTUP_STAGES or stage in stages
                    or set(sample) != {"event", "stage", "elapsed_s"}
                    or type(elapsed) not in (int, float) or not math.isfinite(elapsed) or elapsed < 0):
                raise RuntimeError("startup counters invalid")
            stages[stage] = elapsed
        elif sample.get("event") == "metal_startup":
            values = {key: value for key, value in sample.items() if key != "event"}
            if (set(values) != METAL_STARTUP_FIELDS
                    or any(type(value) is not int or value < 0 for value in values.values())
                    or values["renderer_index"] != len(renderers) or len(renderers) >= 8
                    or values["constructor_total_ns"] != sum(
                        value for key, value in values.items()
                        if key not in ("renderer_index", "constructor_total_ns"))):
                raise RuntimeError("startup counters invalid")
            renderers.append(values)
    return {"stages_elapsed_s": stages, "metal_renderers": renderers}


def collect_frame_counters(output, begin, end):
    samples = []
    for line in output.splitlines():
        try:
            sample = json.loads(line)
        except ValueError:
            continue
        if not isinstance(sample, dict) or sample.get("event") != "frame_counters":
            continue
        elapsed = sample.get("elapsed_s")
        values = sample.get("counters")
        if (type(elapsed) not in (int, float) or not math.isfinite(elapsed)
                or not isinstance(values, dict) or set(values) != FRAME_COUNTER_FIELDS
                or any(type(value) is not int or value < 0 for value in values.values())):
            raise RuntimeError("frame counters unavailable")
        if begin + 0.2 <= elapsed <= end - 0.2:
            samples.append(sample)
    if len(samples) < 2 or samples[-1]["elapsed_s"] <= samples[0]["elapsed_s"]:
        raise RuntimeError("frame counters unavailable")
    first, last = samples[0], samples[-1]
    delta = {name: last["counters"][name] - first["counters"][name] for name in FRAME_COUNTER_FIELDS}
    if any(value < 0 for value in delta.values()):
        raise RuntimeError("frame counters unavailable")
    return {"elapsed_s": last["elapsed_s"] - first["elapsed_s"], "samples": len(samples),
            "delta": delta, "cumulative": last["counters"]}


def run_trial(label, binary, mode, state, warmup, capture, interval, rate, root, reference=None,
              profile_memory=False, frame_counters=False, float_window=False):
    trial_root = root / f"{label}-{mode}-{state}-{time.monotonic_ns()}"
    trial_root.mkdir(mode=0o700)
    environment = os.environ.copy()
    for key, dirname in (("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                         ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache"),
                         ("XDG_RUNTIME_DIR", "runtime")):
        location = trial_root / dirname
        location.mkdir(mode=0o700)
        environment[key] = str(location)
    pid_file = trial_root / "producer-pid.json"
    ready_file = trial_root / "producer-ready.json"
    summary_file = trial_root / "producer-summary.json"
    environment.update({
        "SHELL": str(WORKLOAD),
        "SPACETERM_BENCH_MODE": mode,
        "SPACETERM_BENCH_DURATION": str(warmup + capture + 5),
        "SPACETERM_BENCH_RATE": str(rate),
        "SPACETERM_BENCH_PID_FILE": str(pid_file),
        "SPACETERM_BENCH_READY_FILE": str(ready_file),
        "SPACETERM_BENCH_SUMMARY_FILE": str(summary_file),
        "SPACETERM_BENCH_HOLD_SECONDS": "30" if profile_memory else "0",
        "SPACETERM_BENCH_FRAME_COUNTERS": "1" if frame_counters else "0",
        "SPACETERM_BENCH_ACK": "1",
    })
    started = time.monotonic()
    counter_output = tempfile.TemporaryFile() if frame_counters else None
    try:
        app = subprocess.Popen([str(binary)], cwd=ROOT, env=environment,
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=counter_output if counter_output is not None else subprocess.DEVNULL,
                               start_new_session=True)
    except BaseException:
        if counter_output is not None:
            counter_output.close()
        raise
    samplers = []
    producer_pid = None
    focus_owner = None
    try:
        producer_pid = wait_for_file(pid_file, app, 30)["pid"]
        producer_started_s = time.monotonic() - started
        if mode == "history":
            ready = wait_for_file(ready_file, app, 30)
            if ready != {"event": "history_ready", "emitted_lines": 10000}:
                raise RuntimeError("history fixture did not report the expected output")
        fixture_ready_s = time.monotonic() - started
        time.sleep(warmup)
        if float_window:
            float_owned_window(app.pid)
        set_owned_application_state(app.pid, "focused")
        time.sleep(0.5)
        focused = observe_window_state(app.pid)
        try:
            validate_window_state("focused", reference if reference is not None else focused, focused)
        except RuntimeError:
            print(json.dumps({"event": "invalid_state", "binary": label, "mode": mode,
                              "state": state, "phase": "focused", "app_alive": app.poll() is None,
                              "observation": focused}), flush=True)
            raise
        if state == "hidden":
            observed = focused
            if not observed["hidden"]:
                try:
                    set_owned_application_state(app.pid, state)
                except RuntimeError:
                    print(json.dumps({"event": "invalid_state", "binary": label, "mode": mode,
                                      "state": state, "phase": "hide", "app_alive": app.poll() is None,
                                      "observation": observed}), flush=True)
                    raise
            time.sleep(1)
        elif state == "unfocused":
            focus_owner = start_focus_owner(focused)
            time.sleep(0.5)
        before = observe_window_state(app.pid)
        try:
            validate_window_state(state, focused if state == "focused" else before, before)
            if state == "unfocused" and focused["window_bounds_points"] != before["window_bounds_points"]:
                raise RuntimeError("application window geometry changed during comparison")
        except RuntimeError:
            print(json.dumps({"event": "invalid_state", "binary": label, "mode": mode,
                              "state": state, "phase": "before", "app_alive": app.poll() is None,
                              "observation": before}), flush=True)
            raise
        capture_begin = time.monotonic() - started
        for pid in (app.pid, producer_pid):
            samplers.append(sample(pid, capture, interval))
        app_usage, producer_usage = [collect(item, capture + 10) for item in samplers]
        capture_end = time.monotonic() - started
        frames = None
        startup = None
        if counter_output is not None:
            # The child shares this open file description for stderr. An
            # offset-independent read cannot redirect a concurrent child write.
            descriptor = counter_output.fileno()
            output = os.pread(descriptor, os.fstat(descriptor).st_size, 0).decode("utf-8", errors="replace")
            frames = collect_frame_counters(output, capture_begin, capture_end)
            startup = collect_startup_counters(output)
        after = observe_window_state(app.pid)
        if focus_owner is not None:
            validate_focus_owner(focus_owner, after)
        try:
            validate_window_state(state, before, after)
        except RuntimeError:
            print(json.dumps({"event": "invalid_state", "binary": label, "mode": mode,
                              "state": state, "phase": "after", "app_alive": app.poll() is None,
                              "observation": after}), flush=True)
            raise
        summary = wait_for_file(summary_file, app, 8)
        if summary["mode"] != mode:
            raise RuntimeError("producer ran the wrong fixture")
        validate_consumption(summary)
        memory = None
        settled_usage = None
        if profile_memory:
            # Native memory inspection can suspend the target. Wait for all output
            # first so that inspection cannot change paced producer throughput.
            time.sleep(1)
            settled_sampler = sample(app.pid, 0.1, 0.1)
            samplers.append(settled_sampler)
            settled_usage = collect(settled_sampler, 5)
            memory = memory_profile(app.pid)
            validate_window_state(state, before, observe_window_state(app.pid))
        return {
            "binary": label, "mode": mode, "state": state,
            "producer_started_s": producer_started_s,
            "fixture_ready_s": fixture_ready_s,
            "window_focused": focused,
            "window_before": before, "window_after": after,
            "application": app_usage, "producer": producer_usage,
            "workload": summary,
            **({"memory_profile": memory, "settled_application": settled_usage}
               if memory is not None else {}),
            **({"frame_counters": frames} if frames is not None else {}),
            **({"startup_counters": startup} if startup is not None else {}),
        }
    finally:
        if focus_owner is not None:
            stop_focus_owner(focus_owner)
        for sampler in samplers:
            if sampler.poll() is None:
                sampler.terminate()
                try:
                    sampler.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    sampler.kill()
                    sampler.wait(timeout=5)
        if producer_pid is not None and app.poll() is None:
            parent = subprocess.run(["ps", "-o", "ppid=", "-p", str(producer_pid)],
                                    capture_output=True, text=True, check=False)
            if parent.returncode == 0 and parent.stdout.strip() == str(app.pid):
                try:
                    os.kill(producer_pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
        if app.poll() is None:
            os.killpg(app.pid, signal.SIGTERM)
            try:
                app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(app.pid, signal.SIGKILL)
                app.wait(timeout=5)
        if counter_output is not None:
            counter_output.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--mode", choices=("idle", "partial", "scroll", "history"), default="idle")
    parser.add_argument("--state", choices=("focused", "unfocused", "hidden"), default="focused")
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--warmup", type=float, default=5)
    parser.add_argument("--capture", type=float, default=10)
    parser.add_argument("--interval", type=float, default=0.5)
    parser.add_argument("--rate", type=float, default=60)
    parser.add_argument("--profile-memory", action="store_true")
    parser.add_argument("--frame-counters", action="store_true")
    parser.add_argument("--float-owned-window", action="store_true",
                        help="Use AeroSpace to float only each owned benchmark window")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("application resource comparison requires macOS")
    if (not math.isfinite(args.warmup) or not math.isfinite(args.capture)
            or not 1 <= args.repetitions <= 10 or not 1 <= args.warmup <= 60
            or not 2 <= args.capture <= 300):
        parser.error("repetitions, warmup, or capture is outside the supported range")
    if (not math.isfinite(args.interval) or not math.isfinite(args.rate)
            or not 0.05 <= args.interval <= args.capture or not 1 <= args.rate <= 240):
        parser.error("interval or rate is outside the supported range")
    sources = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    for source in sources.values():
        if not source.is_file() or not os.access(source, os.X_OK):
            parser.error("both source binaries must exist and be executable")
    print(json.dumps({"fixture": "macos_application_resources", "mode": args.mode,
                      "consumption_acknowledgment": "DSR_after_output",
                      "state": args.state,
                      "warmup_s": args.warmup, "capture_s": args.capture,
                      "interval_s": args.interval, "rate": args.rate,
                      "profile_memory": args.profile_memory, "frame_counters": args.frame_counters,
                      "float_owned_window": args.float_owned_window,
                      "memory_reclamation": "disabled" if args.profile_memory else None,
                      "sha256": {label: hashlib.sha256(source.read_bytes()).hexdigest()
                                 for label, source in sources.items()}}), flush=True)
    with tempfile.TemporaryDirectory(prefix="spaceterm-application-bench-") as temporary:
        root = Path(temporary)
        binaries = {label: stage_binary(source, root / label) for label, source in sources.items()}
        reference = None
        reference_workload = None
        for repetition in range(args.repetitions):
            order = ("baseline", "candidate") if repetition % 2 == 0 else ("candidate", "baseline")
            for label in order:
                result = run_trial(label, binaries[label], args.mode, args.state, args.warmup,
                                   args.capture, args.interval, args.rate, root, reference,
                                   args.profile_memory, args.frame_counters, args.float_owned_window)
                if reference is None:
                    reference = result["window_focused"]
                    reference_workload = result["workload"]
                validate_workload(reference_workload, result["workload"])
                result["repetition"] = repetition + 1
                print(json.dumps(result), flush=True)


if __name__ == "__main__":
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "--internal-state":
            print(json.dumps(native_window_state(int(sys.argv[2]))))
        elif len(sys.argv) == 4 and sys.argv[1] == "--internal-action":
            native_set_owned_application_state(int(sys.argv[3]), sys.argv[2])
        else:
            main()
    except (OSError, RuntimeError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        classification = str(error) if str(error) in BENCHMARK_FAILURES else type(error).__name__
        print(f"benchmark failed: {classification}", file=sys.stderr)
        sys.exit(1)
