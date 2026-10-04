#!/usr/bin/env python3
"""Register source-built Development in the desktop's shared application registry.

This metadata belongs in XDG_DATA_HOME/applications, outside SpaceTerm's private
AppDirectories. The desktop owns discovery; each source launch replaces only the
Development identity's entry so its Exec follows the current source prefix.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ElementTree
from pathlib import Path

APPLICATION_ID = "io.github.sadiksaifi.spaceterm-development"
APPLICATION_NAME = "SpaceTerm Development"
ICON_DOCUMENT = (
    Path(__file__).resolve().parent.parent
    / "packaging" / "macos" / "development" / "SpaceTerm Development.icon"
)
SVG_NAMESPACE = "http://www.w3.org/2000/svg"
# The icon document's canvas and the 824-point shape macOS draws inside a 1024-point icon.
ARTWORK_SIZE = 1025
ICON_SIZE = 1024
ICON_SHAPE = 824
# The artwork's outer window outline has 267-point circular corners.
SHAPE_CORNER_RADIUS = 267
# Linear Display P3 to linear sRGB, both with D65 white.
DISPLAY_P3_TO_SRGB = (
    (1.2249401, -0.2249404, 0.0),
    (-0.0420569, 1.0420571, 0.0),
    (-0.0196376, -0.0786361, 1.0982735),
)


def desktop_registry() -> Path:
    override = os.environ.get("XDG_DATA_HOME", "")
    if override and Path(override).is_absolute():
        return Path(override) / "applications"
    home = os.environ.get("HOME", "")
    if not home or not Path(home).is_absolute():
        raise ValueError("an absolute HOME or XDG_DATA_HOME is required")
    return Path(home) / ".local" / "share" / "applications"


def exec_argument(source: str) -> str:
    # Desktop Entry escaping has two layers: Exec argument quoting, then string
    # value escaping. A literal percent must also escape field-code expansion.
    if any(ord(character) < 32 or ord(character) == 127 for character in source):
        raise ValueError("the source path contains characters unsupported by desktop launchers")
    quoted = "".join(
        "\\" + character if character in '\\"`$' else "%%" if character == "%" else character
        for character in source
    )
    return '"' + quoted.replace("\\", "\\\\") + '"'


def exec_value(executable: Path) -> str:
    # GIO validates the first executable before expanding literal percent codes.
    # Keep that token stable and pass the source path as an argument. Python is
    # already required by this launcher, and execv preserves exact argv without
    # shell evaluation or env interpreting an '=' in the source path as a name.
    env = shutil.which("env", path=os.defpath)
    if env is None:
        raise ValueError("the env executable is required")
    return " ".join(exec_argument(argument) for argument in (
        env, "--", "python3", "-I", "-c",
        "import os, sys; os.execv(sys.argv[1], sys.argv[1:])",
        str(executable),
    ))


def icon_value(icon: Path) -> str:
    # An absolute path starts with "/", so only backslashes need string value escaping.
    return str(icon).replace("\\", "\\\\")


def srgb_hex(color: str) -> str:
    space, _, components = color.partition(":")
    if space != "display-p3":
        raise ValueError("the icon uses an unsupported color space")
    encoded = [float(component) for component in components.split(",")[:3]]
    if len(encoded) != 3:
        raise ValueError("the icon has an incomplete color")

    def linear(value: float) -> float:
        return value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4

    def encode(value: float) -> float:
        value = min(max(value, 0.0), 1.0)
        return value * 12.92 if value <= 0.0031308 else 1.055 * value ** (1 / 2.4) - 0.055

    linear_p3 = [linear(value) for value in encoded]
    srgb = [encode(sum(weight * value for weight, value in zip(row, linear_p3))) for row in DISPLAY_P3_TO_SRGB]
    return "#" + "".join(f"{round(value * 255):02X}" for value in srgb)


def development_icon_svg(document: Path) -> str:
    """Flatten the Development icon document's light appearance into one scalable icon."""
    try:
        icon = json.loads((document / "icon.json").read_text(encoding="utf-8"))
        top, bottom = (srgb_hex(color) for color in icon["fill"]["linear-gradient"])
        layers = [layer for group in icon["groups"] for layer in group["layers"] if not layer.get("hidden")]
    except (KeyError, TypeError) as error:
        raise ValueError("the icon document has an unexpected structure") from error

    ElementTree.register_namespace("", SVG_NAMESPACE)
    svg = ElementTree.Element(f"{{{SVG_NAMESPACE}}}svg", {
        "width": str(ICON_SIZE), "height": str(ICON_SIZE), "viewBox": f"0 0 {ICON_SIZE} {ICON_SIZE}",
    })
    gradient = ElementTree.SubElement(
        ElementTree.SubElement(svg, f"{{{SVG_NAMESPACE}}}defs"),
        f"{{{SVG_NAMESPACE}}}linearGradient",
        {"id": "fill", "x1": "0", "y1": "0", "x2": "0", "y2": "1"},
    )
    ElementTree.SubElement(gradient, f"{{{SVG_NAMESPACE}}}stop", {"offset": "0", "stop-color": top})
    ElementTree.SubElement(gradient, f"{{{SVG_NAMESPACE}}}stop", {"offset": "1", "stop-color": bottom})
    margin = (ICON_SIZE - ICON_SHAPE) / 2
    artwork = ElementTree.SubElement(svg, f"{{{SVG_NAMESPACE}}}g", {
        "transform": f"translate({margin:g} {margin:g}) scale({ICON_SHAPE / ARTWORK_SIZE:.6f})",
    })
    ElementTree.SubElement(artwork, f"{{{SVG_NAMESPACE}}}rect", {
        "width": str(ARTWORK_SIZE), "height": str(ARTWORK_SIZE),
        "rx": str(SHAPE_CORNER_RADIUS), "fill": "url(#fill)",
    })
    # The first layer is frontmost. The light appearance draws each layer in white.
    for layer in reversed(layers):
        try:
            source = ElementTree.parse(document / "Assets" / layer["image-name"]).getroot()
        except ElementTree.ParseError as error:
            raise ValueError("the icon has an unreadable layer") from error
        group = ElementTree.SubElement(artwork, f"{{{SVG_NAMESPACE}}}g")
        for element in source:
            for paint in ("fill", "stroke"):
                if element.get(paint) not in (None, "none"):
                    element.set(paint, "#FFFFFF")
            group.append(element)
    return ElementTree.tostring(svg, encoding="unicode") + "\n"


def atomic_write(path: Path, contents: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent, delete=False) as file:
            temporary = Path(file.name)
            file.write(contents)
            file.flush()
            os.fchmod(file.fileno(), 0o644)
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def register(prefix: Path) -> None:
    executable = prefix / "bin" / "spaceterm"
    if not prefix.is_absolute() or not executable.is_file() or not os.access(executable, os.X_OK):
        raise ValueError("a staged Development executable is required")
    destination = desktop_registry() / (APPLICATION_ID + ".desktop")
    entry = prefix / "share" / "applications" / destination.name
    # The source prefix holds the icon, so the entry follows it like Exec does.
    icon = prefix / "share" / "icons" / "hicolor" / "scalable" / "apps" / (APPLICATION_ID + ".svg")
    if any(ord(character) < 32 or ord(character) == 127 for character in str(icon)):
        raise ValueError("the source path contains characters unsupported by desktop launchers")
    atomic_write(icon, development_icon_svg(ICON_DOCUMENT))
    contents = (
        "[Desktop Entry]\n"
        "Type=Application\n"
        f"Name={APPLICATION_NAME}\n"
        f"Exec={exec_value(executable)}\n"
        f"Icon={icon_value(icon)}\n"
        "Terminal=false\n"
        "StartupNotify=true\n"
        f"StartupWMClass={APPLICATION_ID}\n"
        "Categories=System;TerminalEmulator;\n"
    )
    atomic_write(entry, contents)
    subprocess.run(
        ["desktop-file-validate", str(entry)],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    atomic_write(destination, contents)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: register-development-desktop-linux.py PREFIX", file=sys.stderr)
        return 2
    try:
        register(Path(sys.argv[1]))
    except (OSError, ValueError, subprocess.CalledProcessError):
        print("error: could not register the Development desktop entry; run mise run doctor:linux", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
