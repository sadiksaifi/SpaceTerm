#!/usr/bin/env python3
"""Own a small foreground window for an unfocused-visible benchmark.

Launch with stdin=PIPE and retain that pipe for the measurement lifetime. The
single stdout line ``1`` means this process observed itself frontmost, fully
launched and active with one small application window. The caller must validate
this PID and verify that its window does not overlap the measured application.
Closing stdin or terminating this owned process releases the focus owner.
"""

import ctypes
import os
import sys
import threading
import time


class Point(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]


class Size(ctypes.Structure):
    _fields_ = [("width", ctypes.c_double), ("height", ctypes.c_double)]


class Rect(ctypes.Structure):
    _fields_ = [("origin", Point), ("size", Size)]


def watch_owner():
    """Block without a timer until the owner closes its retained pipe."""
    try:
        while sys.stdin.buffer.read(1):
            pass
    finally:
        # AppKit's run method blocks the main Python thread. Exit directly on
        # owner loss; the OS releases this process's native objects and focus.
        os._exit(0)


def main():
    if sys.platform != "darwin" or len(sys.argv) != 1:
        return 1
    threading.Thread(target=watch_owner, name="benchmark-focus-owner", daemon=True).start()

    ctypes.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit")
    foundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
    objc.objc_getClass.argtypes = [ctypes.c_char_p]
    objc.objc_getClass.restype = ctypes.c_void_p
    objc.sel_registerName.argtypes = [ctypes.c_char_p]
    objc.sel_registerName.restype = ctypes.c_void_p

    def message(result, *arguments):
        return ctypes.CFUNCTYPE(
            result, ctypes.c_void_p, ctypes.c_void_p, *arguments
        )(("objc_msgSend", objc))

    object_message = message(ctypes.c_void_p)
    void_message = message(None)
    bool_message = message(ctypes.c_bool)
    pid_message = message(ctypes.c_int)
    count_message = message(ctypes.c_ulong)
    policy_message = message(ctypes.c_bool, ctypes.c_long)
    activation_message = message(None, ctypes.c_bool)
    object_argument_message = message(None, ctypes.c_void_p)
    window_init_message = message(
        ctypes.c_void_p, Rect, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_bool
    )
    selector = objc.sel_registerName
    pool = object_message(objc.objc_getClass(b"NSAutoreleasePool"), selector(b"new"))
    app = object_message(objc.objc_getClass(b"NSApplication"), selector(b"sharedApplication"))
    if not app or not policy_message(app, selector(b"setActivationPolicy:"), 0):
        return 1  # NSApplicationActivationPolicyRegular
    window = object_message(objc.objc_getClass(b"NSWindow"), selector(b"alloc"))
    window = window_init_message(
        window, selector(b"initWithContentRect:styleMask:backing:defer:"),
        Rect(Point(8, 32), Size(120, 60)), 1, 2, False,
    )  # NSWindowStyleMaskTitled, NSBackingStoreBuffered
    if not window:
        return 1
    menu = object_message(objc.objc_getClass(b"NSMenu"), selector(b"new"))
    object_argument_message(app, selector(b"setMainMenu:"), menu)

    workspace = object_message(objc.objc_getClass(b"NSWorkspace"), selector(b"sharedWorkspace"))
    owned = object_message(objc.objc_getClass(b"NSRunningApplication"), selector(b"currentApplication"))
    if not workspace or not owned:
        return 1

    timer_callback_type = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p)
    foundation.CFAbsoluteTimeGetCurrent.argtypes = []
    foundation.CFAbsoluteTimeGetCurrent.restype = ctypes.c_double
    foundation.CFRunLoopGetMain.argtypes = []
    foundation.CFRunLoopGetMain.restype = ctypes.c_void_p
    foundation.CFRunLoopTimerCreate.argtypes = [
        ctypes.c_void_p, ctypes.c_double, ctypes.c_double, ctypes.c_ulong,
        ctypes.c_long, timer_callback_type, ctypes.c_void_p,
    ]
    foundation.CFRunLoopTimerCreate.restype = ctypes.c_void_p
    foundation.CFRunLoopAddTimer.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p]
    foundation.CFRunLoopAddTimer.restype = None
    foundation.CFRunLoopTimerInvalidate.argtypes = [ctypes.c_void_p]
    foundation.CFRunLoopTimerInvalidate.restype = None
    foundation.CFRelease.argtypes = [ctypes.c_void_p]
    foundation.CFRelease.restype = None
    common_modes = ctypes.c_void_p.in_dll(foundation, "kCFRunLoopCommonModes").value
    deadline = time.monotonic() + 10
    requested_activation = False

    @timer_callback_type
    def check_readiness(timer, _context):
        nonlocal requested_activation
        try:
            if time.monotonic() >= deadline:
                os._exit(1)
            if not bool_message(owned, selector(b"isFinishedLaunching")):
                return
            if not requested_activation:
                requested_activation = True
                # Both operations target our own native objects. The caller
                # verifies non-overlap before beginning its measured interval.
                object_argument_message(window, selector(b"makeKeyAndOrderFront:"), None)
                activation_message(app, selector(b"activateIgnoringOtherApps:"), True)
            frontmost = object_message(workspace, selector(b"frontmostApplication"))
            if not frontmost or pid_message(frontmost, selector(b"processIdentifier")) != os.getpid():
                return
            if not bool_message(app, selector(b"isActive")):
                return
            windows = object_message(app, selector(b"windows"))
            if count_message(windows, selector(b"count")) != 1:
                os._exit(1)
            if not bool_message(window, selector(b"isVisible")):
                os._exit(1)
            # No polling remains during the actual measured interval. The run
            # loop stays available for ordinary AppKit lifecycle events.
            foundation.CFRunLoopTimerInvalidate(timer)
            os.write(sys.stdout.fileno(), b"1\n")
        except BaseException:
            # ctypes callbacks must not unwind into AppKit or print raw errors.
            os._exit(1)

    timer = foundation.CFRunLoopTimerCreate(
        None, foundation.CFAbsoluteTimeGetCurrent() + 0.05, 0.05,
        0, 0, check_readiness, None,
    )
    if not timer:
        return 1
    try:
        foundation.CFRunLoopAddTimer(foundation.CFRunLoopGetMain(), timer, common_modes)
        void_message(app, selector(b"run"))
    finally:
        foundation.CFRunLoopTimerInvalidate(timer)
        foundation.CFRelease(timer)
        if pool:
            void_message(pool, selector(b"drain"))
    return 1  # Unexpected run-loop termination is not a valid focus owner.


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception:
        raise SystemExit(1) from None
