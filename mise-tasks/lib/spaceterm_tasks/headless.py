"""Reject desktop dependencies reachable from headless workspace crates.

Cargo metadata includes dependencies for all targets, including build and dev
edges. A Linux run therefore also checks dormant macOS dependency paths. Desktop
roots may use GUI packages; only reachability from a headless root is rejected.
"""

import json
import re
import subprocess
from collections import deque

from spaceterm_tasks import ROOT, TaskError, main

# Extend this list when another portable composition crate becomes mandatory.
REQUIRED_CRATES = (
    "spaceterm-core",
    "spaceterm-client",
    "spaceterm-server",
    "spaceterm-host",
    "spaceterm-protocol",
    "spaceterm-terminal",
)

# Normalize underscores so package spelling and Cargo aliases cannot hide a family.
FORBIDDEN_FAMILIES = (
    "gpui",
    "objc",
    "objc2",
    "cocoa",
    "appkit",
    "app-kit",
    "core-foundation",
    "font",
    "fonts",
    "read-fonts",
    "write-fonts",
    "skrifa",
    "swash",
    "harfbuzz",
    "pango",
    "fontconfig",
    "yeslogic-fontconfig",
    "freetype",
    "freetype2",
    "freetype-rs",
    "freetype-sys",
    "libfreetype",
    "core-text",
    "dwrote",
    "directwrite",
    "cosmic-text",
    "rustybuzz",
    "ttf-parser",
    "fontdb",
    "fontdue",
    "ab-glyph",
    "x11",
    "x11rb",
    "x11-dl",
    "xcb",
    "xkbcommon",
    "wayland",
    "winit",
    "glfw",
    "sdl2",
    "sdl3",
    "orbclient",
    "raw-window-handle",
    "smithay",
    "calloop-wayland",
    "wgpu",
    "metal",
    "blade",
    "naga",
    "ash",
    "vulkano",
    "gpu-alloc",
    "gpu-descriptor",
    "gfx",
    "gl",
    "egl",
    "glx",
    "cgl",
    "gles",
    "opengl",
    "glow",
    "glium",
    "glutin",
    "khronos-egl",
    "softbuffer",
    "pixels",
    "skia",
    "vello",
    "gtk",
    "gtk4",
    "gdk",
    "gdk4",
    "gsk4",
    "gio",
    "ashpd",
    "arboard",
    "clipboard",
    "copypasta",
    "wl-clipboard",
    "rfd",
    "tray-icon",
    "ksni",
    "accesskit-atspi",
    "accesskit-macos",
    "accesskit-unix",
    "accesskit-winit",
)
PACKAGE_NAME = re.compile(r"[A-Za-z0-9_-]+\Z")


def forbidden_package(name):
    """Classify package names, not dependency aliases or package/source IDs."""
    normalized = name.lower().replace("_", "-")
    return any(
        normalized == family or normalized.startswith(f"{family}-") for family in FORBIDDEN_FAMILIES
    )


def check_metadata(metadata):
    """Check a resolved Cargo graph and return checked workspace crate names.

    Missing mandatory roots or incomplete graphs fail closed. Diagnostics report
    only Cargo package names, never manifest paths, package IDs or native errors.
    """
    try:
        if metadata["version"] != 1 or not isinstance(metadata["resolve"], dict):
            raise ValueError
        packages = metadata["packages"]
        names = {package["id"]: package["name"] for package in packages}
        if len(names) != len(packages) or any(
            not isinstance(name, str) or PACKAGE_NAME.fullmatch(name) is None
            for name in names.values()
        ):
            raise ValueError
        members = metadata["workspace_members"]
        if not isinstance(members, list) or any(member not in names for member in members):
            raise ValueError
        roots = {}
        for member in members:
            name = names[member]
            if name in REQUIRED_CRATES:
                if name in roots:
                    raise ValueError
                roots[name] = member
        missing = [name for name in REQUIRED_CRATES if name not in roots]
        if missing:
            raise TaskError(f"headless workspace crates missing: {', '.join(missing)}")
        nodes = metadata["resolve"]["nodes"]
        graph = {node["id"]: node["dependencies"] for node in nodes}
        if len(graph) != len(nodes) or any(
            package not in names
            or not isinstance(dependencies, list)
            or any(
                dependency not in names or dependency not in graph for dependency in dependencies
            )
            for package, dependencies in graph.items()
        ):
            raise ValueError
        if any(root not in graph for root in roots.values()):
            raise ValueError
    except (KeyError, TypeError, ValueError, AttributeError):
        raise TaskError("Cargo returned an incomplete dependency graph") from None

    checked = list(REQUIRED_CRATES)
    for name in checked:
        root = roots[name]
        parents = {root: None}
        queue = deque([root])
        while queue:
            package = queue.popleft()
            if forbidden_package(names[package]):
                chain = []
                current = package
                while current is not None:
                    chain.append(names[current])
                    current = parents[current]
                raise TaskError(f"headless dependency forbidden: {' -> '.join(reversed(chain))}")
            for dependency in sorted(graph[package], key=lambda package_id: names[package_id]):
                if dependency not in parents:
                    parents[dependency] = package
                    queue.append(dependency)
    return tuple(checked)


def check_workspace():
    """Resolve all target paths without compiling or exposing Cargo's native errors."""
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--all-features"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        raise TaskError("Cargo could not resolve the headless dependency graph")
    try:
        metadata = json.loads(result.stdout)
    except (ValueError, TypeError):
        raise TaskError("Cargo returned an invalid dependency graph") from None
    checked = check_metadata(metadata)
    print(f"headless dependencies checked: {', '.join(checked)}")


if __name__ == "__main__":
    main(check_workspace)
