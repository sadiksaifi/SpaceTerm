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
            raise RuntimeError("could not hide the owned application")
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
        if state == "focused" and len(bounds) != 1:
            raise RuntimeError("owned application window count was not one")
        if state == "hidden" and (observation["frontmost"] or bounds or not observation["hidden"]):
            raise RuntimeError("hidden application state was not verified")
    if state == "focused" and before["window_bounds_points"] != after["window_bounds_points"]:
        raise RuntimeError("application window geometry changed during comparison")


def wait_for_file(path, app, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return json.loads(path.read_text(encoding="utf-8"))
        if app.poll() is not None:
            raise RuntimeError("application exited before the workload reported readiness")
        time.sleep(0.05)
    raise RuntimeError("timed out waiting for the owned workload")


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


def run_trial(label, binary, mode, state, warmup, capture, interval, rate, root, reference=None):
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
    })
    started = time.monotonic()
    app = subprocess.Popen([str(binary)], cwd=ROOT, env=environment,
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, start_new_session=True)
    samplers = []
    producer_pid = None
    completed = False
    try:
        producer_pid = wait_for_file(pid_file, app, 30)["pid"]
        producer_started_s = time.monotonic() - started
        if mode == "history":
            ready = wait_for_file(ready_file, app, 30)
            if ready != {"event": "history_ready", "emitted_lines": 10000}:
                raise RuntimeError("history fixture did not report the expected output")
        fixture_ready_s = time.monotonic() - started
        time.sleep(warmup)
        set_owned_application_state(app.pid, "focused")
        time.sleep(0.5)
        if state == "hidden":
            observed = observe_window_state(app.pid)
            if not observed["hidden"]:
                try:
                    set_owned_application_state(app.pid, state)
                except RuntimeError:
                    print(json.dumps({"event": "invalid_state", "binary": label, "mode": mode,
                                      "state": state, "phase": "hide", "app_alive": app.poll() is None,
                                      "observation": observed}), flush=True)
                    raise
            time.sleep(1)
        before = observe_window_state(app.pid)
        try:
            validate_window_state(state, reference if reference is not None else before, before)
        except RuntimeError:
            print(json.dumps({"event": "invalid_state", "binary": label, "mode": mode,
                              "state": state, "phase": "before", "app_alive": app.poll() is None,
                              "observation": before}), flush=True)
            raise
        for pid in (app.pid, producer_pid):
            samplers.append(sample(pid, capture, interval))
        app_usage, producer_usage = [collect(item, capture + 10) for item in samplers]
        after = observe_window_state(app.pid)
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
        completed = True
        return {
            "binary": label, "mode": mode, "state": state,
            "producer_started_s": producer_started_s,
            "fixture_ready_s": fixture_ready_s,
            "window_before": before, "window_after": after,
            "application": app_usage, "producer": producer_usage,
            "workload": summary,
        }
    finally:
        for sampler in samplers:
            if sampler.poll() is None:
                sampler.terminate()
                try:
                    sampler.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    sampler.kill()
                    sampler.wait(timeout=5)
        if not completed and producer_pid is not None and app.poll() is None:
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--mode", choices=("idle", "partial", "scroll", "history"), default="idle")
    parser.add_argument("--state", choices=("focused", "hidden"), default="focused")
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--warmup", type=float, default=5)
    parser.add_argument("--capture", type=float, default=10)
    parser.add_argument("--interval", type=float, default=0.5)
    parser.add_argument("--rate", type=float, default=60)
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
                      "state": args.state,
                      "warmup_s": args.warmup, "capture_s": args.capture,
                      "interval_s": args.interval, "rate": args.rate,
                      "sha256": {label: hashlib.sha256(source.read_bytes()).hexdigest()
                                 for label, source in sources.items()}}), flush=True)
    with tempfile.TemporaryDirectory(prefix="spaceterm-application-bench-") as temporary:
        root = Path(temporary)
        binaries = {label: stage_binary(source, root / label) for label, source in sources.items()}
        reference = None
        for repetition in range(args.repetitions):
            order = ("baseline", "candidate") if repetition % 2 == 0 else ("candidate", "baseline")
            for label in order:
                result = run_trial(label, binaries[label], args.mode, args.state, args.warmup,
                                   args.capture, args.interval, args.rate, root, reference)
                if reference is None:
                    reference = result["window_before"]
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
