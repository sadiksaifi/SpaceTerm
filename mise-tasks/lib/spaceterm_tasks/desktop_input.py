"""Post keyboard and mouse events and read window bounds for bench:desktop:macos, without AppKit.

bench:desktop:macos runs this file with a Python that holds the macOS Accessibility permission, which
posting events requires. It answers one JSON request per line on standard input with one JSON
line on standard output. Only `read_text` uses the Accessibility API on another application: it
reads the focused terminal Pane's text, which switches that application's accessibility work on.
No other request queries an application, so a benchmark with Accessibility off stays off.
`read_text` reports the text's length, never the text.

Standard library and ctypes only, so any CPython 3.11 or later can run it.
"""

import ctypes
import json
import sys
import time

CG = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
CF = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
AX = ctypes.CDLL("/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices")


class Point(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]


class Rect(ctypes.Structure):
    _fields_ = [
        ("x", ctypes.c_double),
        ("y", ctypes.c_double),
        ("width", ctypes.c_double),
        ("height", ctypes.c_double),
    ]


void_p = ctypes.c_void_p
CF.CFRelease.argtypes = [void_p]
CF.CFRetain.argtypes = [void_p]
CF.CFRetain.restype = void_p
CF.CFArrayGetCount.argtypes = [void_p]
CF.CFArrayGetCount.restype = ctypes.c_long
CF.CFArrayGetValueAtIndex.argtypes = [void_p, ctypes.c_long]
CF.CFArrayGetValueAtIndex.restype = void_p
CF.CFDictionaryGetValue.argtypes = [void_p, void_p]
CF.CFDictionaryGetValue.restype = void_p
CF.CFNumberGetValue.argtypes = [void_p, ctypes.c_int, void_p]
CF.CFNumberGetValue.restype = ctypes.c_bool
CF.CFBooleanGetValue.argtypes = [void_p]
CF.CFBooleanGetValue.restype = ctypes.c_bool
CF.CFStringCreateWithCString.argtypes = [void_p, ctypes.c_char_p, ctypes.c_uint32]
CF.CFStringCreateWithCString.restype = void_p
CF.CFStringGetLength.argtypes = [void_p]
CF.CFStringGetLength.restype = ctypes.c_long
CF.CFStringGetCString.argtypes = [void_p, ctypes.c_char_p, ctypes.c_long, ctypes.c_uint32]
CF.CFStringGetCString.restype = ctypes.c_bool
CF.CFGetTypeID.argtypes = [void_p]
CF.CFGetTypeID.restype = ctypes.c_ulong
CF.CFStringGetTypeID.restype = ctypes.c_ulong
CG.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
CG.CGWindowListCopyWindowInfo.restype = void_p
CG.CGRectMakeWithDictionaryRepresentation.argtypes = [void_p, ctypes.POINTER(Rect)]
CG.CGRectMakeWithDictionaryRepresentation.restype = ctypes.c_bool
CG.CGMainDisplayID.restype = ctypes.c_uint32
CG.CGDisplayBounds.argtypes = [ctypes.c_uint32]
CG.CGDisplayBounds.restype = Rect
CG.CGEventSourceCreate.argtypes = [ctypes.c_int32]
CG.CGEventSourceCreate.restype = void_p
CG.CGEventCreateKeyboardEvent.argtypes = [void_p, ctypes.c_uint16, ctypes.c_bool]
CG.CGEventCreateKeyboardEvent.restype = void_p
CG.CGEventCreateMouseEvent.argtypes = [void_p, ctypes.c_uint32, Point, ctypes.c_uint32]
CG.CGEventCreateMouseEvent.restype = void_p
CG.CGEventSetFlags.argtypes = [void_p, ctypes.c_uint64]
CG.CGEventPost.argtypes = [ctypes.c_uint32, void_p]
CG.CGPreflightPostEventAccess.restype = ctypes.c_bool
AX.AXIsProcessTrusted.restype = ctypes.c_bool
AX.AXUIElementCreateApplication.argtypes = [ctypes.c_int]
AX.AXUIElementCreateApplication.restype = void_p
AX.AXUIElementCopyAttributeValue.argtypes = [void_p, void_p, ctypes.POINTER(void_p)]
AX.AXUIElementCopyAttributeValue.restype = ctypes.c_int32

UTF8 = 0x08000100
NUMBER_SINT64 = 4
ON_SCREEN_ONLY = 1 << 0
EXCLUDE_DESKTOP = 1 << 4
HID_SYSTEM_STATE = 1
HID_EVENT_TAP = 0
LEFT_DOWN, LEFT_UP, MOUSE_MOVED, LEFT_DRAGGED = 1, 2, 5, 6
FLAGS = {"shift": 1 << 17, "control": 1 << 18, "option": 1 << 19, "command": 1 << 20}
MODIFIER_KEYS = {"control": 59, "command": 55, "shift": 56, "option": 58}
# US layout virtual key codes, as the bench:latency pixel probe types them.
KEYS = {
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11,
    "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17, "o": 31, "u": 32, "i": 34, "p": 35,
    "l": 37, "j": 38, "k": 40, "n": 45, "m": 46, " ": 49, "1": 18, "2": 19, "3": 20, "4": 21,
    "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29, "-": 27, "=": 24, ";": 41, "'": 39,
    ",": 43, ".": 47, "/": 44, "\\": 42, "[": 33, "]": 30, "`": 50,
    "return": 36, "tab": 48, "escape": 53,
}  # fmt: skip
SHIFTED = dict(zip('~!@#$%^&*()_+{}|:"<>?', "`1234567890-=[]\\;',./", strict=True))
SOURCE = CG.CGEventSourceCreate(HID_SYSTEM_STATE)


def cfstring(text):
    return CF.CFStringCreateWithCString(None, text.encode(), UTF8)


WINDOW_KEYS = {
    name: cfstring(name)
    for name in (
        "kCGWindowOwnerPID",
        "kCGWindowNumber",
        "kCGWindowLayer",
        "kCGWindowBounds",
        "kCGWindowIsOnscreen",
    )
}


def number(dictionary, key):
    value = CF.CFDictionaryGetValue(dictionary, WINDOW_KEYS[key])
    result = ctypes.c_int64()
    if value and CF.CFNumberGetValue(value, NUMBER_SINT64, ctypes.byref(result)):
        return result.value
    return None


def windows(pid):
    """The process's on-screen windows, frontmost first, with bounds in points."""
    listing = CG.CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0)
    result = []
    try:
        for index in range(CF.CFArrayGetCount(listing)):
            info = CF.CFArrayGetValueAtIndex(listing, index)
            if number(info, "kCGWindowOwnerPID") != pid:
                continue
            bounds = Rect()
            dictionary = CF.CFDictionaryGetValue(info, WINDOW_KEYS["kCGWindowBounds"])
            if not dictionary or not CG.CGRectMakeWithDictionaryRepresentation(
                dictionary, ctypes.byref(bounds)
            ):
                continue
            result.append(
                {
                    "number": number(info, "kCGWindowNumber"),
                    "layer": number(info, "kCGWindowLayer"),
                    "bounds": [bounds.x, bounds.y, bounds.width, bounds.height],
                }
            )
    finally:
        CF.CFRelease(listing)
    return result


def post_key(code, modifiers=()):
    held = 0
    for modifier in modifiers:
        held |= FLAGS[modifier]
        event = CG.CGEventCreateKeyboardEvent(SOURCE, MODIFIER_KEYS[modifier], True)
        CG.CGEventSetFlags(event, held)
        CG.CGEventPost(HID_EVENT_TAP, event)
        CF.CFRelease(event)
        time.sleep(0.015)
    for down in (True, False):
        event = CG.CGEventCreateKeyboardEvent(SOURCE, code, down)
        CG.CGEventSetFlags(event, held)
        CG.CGEventPost(HID_EVENT_TAP, event)
        CF.CFRelease(event)
    for modifier in reversed(modifiers):
        time.sleep(0.015)
        held &= ~FLAGS[modifier]
        event = CG.CGEventCreateKeyboardEvent(SOURCE, MODIFIER_KEYS[modifier], False)
        CG.CGEventSetFlags(event, held)
        CG.CGEventPost(HID_EVENT_TAP, event)
        CF.CFRelease(event)


def type_text(text):
    for character in text:
        if character == "\n":
            post_key(KEYS["return"])
        elif character in KEYS:
            post_key(KEYS[character])
        elif character in SHIFTED:
            post_key(KEYS[SHIFTED[character]], ("shift",))
        elif character.isupper() and character.lower() in KEYS:
            post_key(KEYS[character.lower()], ("shift",))
        else:
            raise ValueError("a character outside the US keyboard layout")
        time.sleep(0.015)


def paced(key, count, rate):
    """Press `key` `count` times at `rate` per second and report how late the presses were."""
    started = time.monotonic()
    late = 0.0
    for index in range(count):
        due = started + index / rate
        time.sleep(max(0, due - time.monotonic()))
        late = max(late, time.monotonic() - due)
        post_key(KEYS[key])
    time.sleep(max(0, started + count / rate - time.monotonic()))
    return {"count": count, "rate": rate, "wall_s": time.monotonic() - started, "max_late_s": late}


def mouse(kind, point):
    event = CG.CGEventCreateMouseEvent(SOURCE, kind, Point(*point), 0)
    CG.CGEventPost(HID_EVENT_TAP, event)
    CF.CFRelease(event)


def drag(start, end, steps=12, pause=0.008):
    """Press at `start`, move to `end` in `steps` and release, as a pointer resize does."""
    mouse(MOUSE_MOVED, start)
    time.sleep(0.05)
    mouse(LEFT_DOWN, start)
    time.sleep(0.05)
    for step in range(1, steps + 1):
        fraction = step / steps
        point = (
            start[0] + (end[0] - start[0]) * fraction,
            start[1] + (end[1] - start[1]) * fraction,
        )
        mouse(LEFT_DRAGGED, point)
        time.sleep(pause)
    mouse(LEFT_UP, end)


def copy_attribute(element, name):
    key = cfstring(name)
    value = void_p()
    try:
        error = AX.AXUIElementCopyAttributeValue(element, key, ctypes.byref(value))
    finally:
        CF.CFRelease(key)
    return value.value if error == 0 else None


def string(value):
    if not value or CF.CFGetTypeID(value) != CF.CFStringGetTypeID():
        return None
    buffer = ctypes.create_string_buffer(256)
    return (
        buffer.value.decode() if CF.CFStringGetCString(value, buffer, len(buffer), UTF8) else None
    )


def text_area(element, depth=0, budget=None):
    """The first AXTextArea at or under `element`, searched depth first within a node budget.

    The caller owns the returned reference.
    """
    budget = budget if budget is not None else [4000]
    budget[0] -= 1
    role_value = copy_attribute(element, "AXRole")
    role = string(role_value)
    if role_value:
        CF.CFRelease(role_value)
    if role == "AXTextArea":
        return CF.CFRetain(element)
    if depth >= 40 or budget[0] <= 0:
        return None
    children = copy_attribute(element, "AXChildren")
    if not children:
        return None
    try:
        for index in range(CF.CFArrayGetCount(children)):
            found = text_area(CF.CFArrayGetValueAtIndex(children, index), depth + 1, budget)
            if found:
                return found
    finally:
        CF.CFRelease(children)
    return None


def read_text(pid):
    """Reads the focused Pane's text through the Accessibility API and returns its length."""
    application = AX.AXUIElementCreateApplication(pid)
    try:
        focused = copy_attribute(application, "AXFocusedUIElement")
        window = None if focused else copy_attribute(application, "AXFocusedWindow")
        root = focused or window
        if not root:
            return {"read": False, "reason": "no focused element"}
        try:
            area = text_area(root)
            if not area and focused:
                window = copy_attribute(application, "AXFocusedWindow")
                area = text_area(window) if window else None
            if not area:
                return {"read": False, "reason": "no text area"}
            value = copy_attribute(area, "AXValue")
            CF.CFRelease(area)
            if not value:
                return {"read": False, "reason": "no value"}
            length = (
                CF.CFStringGetLength(value)
                if CF.CFGetTypeID(value) == CF.CFStringGetTypeID()
                else 0
            )
            CF.CFRelease(value)
            return {"read": True, "length": length}
        finally:
            CF.CFRelease(root)
            if window and window != root:
                CF.CFRelease(window)
    finally:
        CF.CFRelease(application)


def handle(request):
    operation = request["op"]
    if operation == "check":
        return {"trusted": bool(AX.AXIsProcessTrusted() and CG.CGPreflightPostEventAccess())}
    if operation == "windows":
        return {"windows": windows(request["pid"])}
    if operation == "display":
        bounds = CG.CGDisplayBounds(CG.CGMainDisplayID())
        return {"bounds": [bounds.x, bounds.y, bounds.width, bounds.height]}
    if operation == "key":
        post_key(KEYS[request["key"]], tuple(request.get("modifiers", ())))
        return {}
    if operation == "type":
        type_text(request["text"])
        return {}
    if operation == "paced":
        return paced(request["key"], request["count"], request["rate"])
    if operation == "drag":
        drag(request["from"], request["to"])
        return {}
    if operation == "read_text":
        return read_text(request["pid"])
    raise ValueError("unknown request")


def main():
    for line in sys.stdin:
        try:
            response = {"ok": True, **handle(json.loads(line))}
        except (KeyError, ValueError) as error:
            response = {"ok": False, "error": type(error).__name__}
        sys.stdout.write(json.dumps(response) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
