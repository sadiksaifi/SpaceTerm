"""Generate every SpaceTerm brand asset in assets/brand from one mark and one icon document.

The mark's geometry lives here. Colors, glass and appearances live in the Icon Composer document
`assets/brand/apple/SpaceTerm.icon/icon.json`, which people edit in Icon Composer. Generation
writes the document's layer artwork and every platform's files, then records a manifest of
source and output digests. Checking compares the tree with that manifest and needs no renderer,
so it runs on any host. Generation needs macOS: Icon Composer's ictool renders the Apple
previews, resvg rasterizes everything else, and fonttools outlines the social preview's text.
"""

import hashlib
import json
import math
import shutil
import struct
import subprocess
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

BRAND = Path("assets/brand")
DOCUMENT = BRAND / "apple" / "SpaceTerm.icon"
MANIFEST = BRAND / "manifest.json"
APPLICATION_ID = "io.github.sadiksaifi.spaceterm"
# Files people write by hand. Every other file under assets/brand is generated.
HANDWRITTEN = (BRAND / "README.md", DOCUMENT / "icon.json")
WORDMARK_FONT = Path("assets/fonts/inter/Inter-SemiBold.otf")
TAGLINE_FONT = Path("assets/fonts/inter/Inter-Regular.otf")
ICTOOL = Path(
    "/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool"
)

# ----------------------------------------------------------------------------------------- mark
# The mark sits on a 100-unit grid centered at (50, 50): a sun, two orbits, and a planet on each.
CENTER = (50.0, 50.0)
SUN_RADIUS = 14.0
ORBIT_STROKE = 7.0
# The gap each orbit leaves around its planet, so the planet reads as riding on it.
PLANET_CLEARANCE = 4.5
# (orbit radius, planet center, planet radius)
ORBITS = (
    (27.5, (70.436, 31.599), 6.2),
    (43.5, (13.937, 74.325), 7.4),
)
# The status item's attention badge sits on the outer orbit's upper right.
BADGE = ((85.0, 15.0), 8.5)
BADGE_CLEARANCE = 4.0


def number(value):
    text = f"{value:.3f}".rstrip("0").rstrip(".")
    return "0" if text == "-0" else text


def disc(center, radius):
    x, y = center
    return (
        f"M{number(x - radius)} {number(y)}"
        f"A{number(radius)} {number(radius)} 0 1 0 {number(x + radius)} {number(y)}"
        f"A{number(radius)} {number(radius)} 0 1 0 {number(x - radius)} {number(y)}Z"
    )


def ring(radius, cuts):
    """An orbit of ORBIT_STROKE width, broken by each (center, radius) circle in `cuts`.

    Each piece runs along the outer edge in increasing angle, follows the next cut's circle in,
    returns along the inner edge, and follows the previous cut's circle out.
    """
    outer, inner = radius + ORBIT_STROKE / 2, radius - ORBIT_STROKE / 2

    def half_angle(edge, cut_distance, cut_radius):
        cosine = (edge * edge + cut_distance * cut_distance - cut_radius * cut_radius) / (
            2 * edge * cut_distance
        )
        return math.acos(max(-1.0, min(1.0, cosine)))

    gaps = []
    for (cx, cy), cut_radius in cuts:
        distance = math.hypot(cx - CENTER[0], cy - CENTER[1])
        angle = math.atan2(cy - CENTER[1], cx - CENTER[0])
        gaps.append(
            (
                angle,
                half_angle(outer, distance, cut_radius),
                half_angle(inner, distance, cut_radius),
                cut_radius,
            )
        )
    gaps.sort()

    def point(edge, angle):
        return (
            number(CENTER[0] + edge * math.cos(angle)),
            number(CENTER[1] + edge * math.sin(angle)),
        )

    pieces = []
    for index, (angle, outer_half, inner_half, _) in enumerate(gaps):
        following = gaps[(index + 1) % len(gaps)]
        end_angle = following[0] + (2 * math.pi if index + 1 == len(gaps) else 0)
        start_outer, end_outer = angle + outer_half, end_angle - following[1]
        start_inner, end_inner = angle + inner_half, end_angle - following[2]
        large_outer = int(end_outer - start_outer > math.pi)
        large_inner = int(end_inner - start_inner > math.pi)
        cut_after, cut_before = number(following[3]), number(gaps[index][3])
        pieces.append(
            "M{} {}".format(*point(outer, start_outer))
            + f"A{number(outer)} {number(outer)} 0 {large_outer} 1 "
            + "{} {}".format(*point(outer, end_outer))
            + f"A{cut_after} {cut_after} 0 0 0 "
            + "{} {}".format(*point(inner, end_inner))
            + f"A{number(inner)} {number(inner)} 0 {large_inner} 0 "
            + "{} {}".format(*point(inner, start_inner))
            + f"A{cut_before} {cut_before} 0 0 0 "
            + "{} {}Z".format(*point(outer, start_outer))
        )
    return "".join(pieces)


def orbits_path(badge=False):
    paths = []
    for radius, planet, planet_radius in ORBITS:
        cuts = [(planet, planet_radius + PLANET_CLEARANCE)]
        if badge and radius == ORBITS[-1][0]:
            cuts.append((BADGE[0], BADGE[1] + BADGE_CLEARANCE))
        paths.append(ring(radius, cuts))
    return "".join(paths)


SUN = disc(CENTER, SUN_RADIUS)
PLANETS = "".join(disc(planet, radius) for _, planet, radius in ORBITS)


def glyph_path(badge=False):
    """The whole mark as one path, for one-color uses."""
    return orbits_path(badge) + SUN + PLANETS + (disc(*BADGE) if badge else "")


# --------------------------------------------------------------------------------------- colors
@dataclass(frozen=True)
class Fill:
    """A solid color or a top-to-bottom gradient, as the icon document defines it."""

    top: str
    bottom: str

    def gradient(self, identifier):
        return (
            f'<linearGradient id="{identifier}" x1="0" y1="0" x2="0" y2="1">'
            f'<stop offset="0" stop-color="{self.top}"/>'
            f'<stop offset="1" stop-color="{self.bottom}"/></linearGradient>'
        )


@dataclass(frozen=True)
class Palette:
    background: Fill
    orbits: Fill
    sun: Fill
    planets: Fill


def hex_color(value):
    space, _, components = value.partition(":")
    if space != "srgb":
        raise TaskError("the icon document must define its colors in sRGB")
    red, green, blue = (float(component) for component in components.split(",")[:3])
    return "#" + "".join(f"{round(channel * 255):02X}" for channel in (red, green, blue))


def fill(specializations, appearance):
    value = next(
        (entry["value"] for entry in specializations if entry.get("appearance") == appearance),
        None,
    )
    if value is None:
        raise TaskError(f"the icon document has no {appearance or 'light'} fill")
    if "solid" in value:
        color = hex_color(value["solid"])
        return Fill(color, color)
    top, bottom = (hex_color(color) for color in value["linear-gradient"])
    return Fill(top, bottom)


def palettes(document):
    """The light and dark palettes of an icon document's parsed icon.json."""
    try:
        layers = {
            layer["name"]: layer["fill-specializations"]
            for group in document["groups"]
            for layer in group["layers"]
        }

        def palette(appearance):
            return Palette(
                fill(document["fill-specializations"], appearance),
                fill(layers["Orbits"], appearance),
                fill(layers["Sun"], appearance),
                fill(layers["Planets"], appearance),
            )

        return palette(None), palette("dark")
    except (KeyError, TypeError, ValueError) as error:
        raise TaskError("the icon document has an unexpected structure") from error


# ------------------------------------------------------------------------------------ drawings
def svg(width, height, body, defs="", view_box=None):
    view_box = view_box or f"0 0 {number(width)} {number(height)}"
    defs = f"<defs>{defs}</defs>" if defs else ""
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{number(width)}" '
        f'height="{number(height)}" viewBox="{view_box}">{defs}{body}</svg>\n'
    )


def place(center_x, center_y, scale):
    """A transform that puts the grid's center at (center_x, center_y), `scale` per unit."""
    return (
        f"translate({number(center_x - CENTER[0] * scale)} {number(center_y - CENTER[1] * scale)}) "
        f"scale({number(scale)})"
    )


def mark(palette, transform, prefix="m"):
    """The full-color mark. Gradients span each shape's own bounds, as in Icon Composer."""
    defs = (
        palette.orbits.gradient(f"{prefix}o")
        + palette.sun.gradient(f"{prefix}s")
        + palette.planets.gradient(f"{prefix}p")
    )
    body = (
        f'<g transform="{transform}">'
        f'<path d="{orbits_path()}" fill="url(#{prefix}o)"/>'
        f'<path d="{SUN}" fill="url(#{prefix}s)"/>'
        f'<path d="{PLANETS}" fill="url(#{prefix}p)"/></g>'
    )
    return defs, body


# The Icon Composer layers place the grid at 80 % of a 1024-point canvas.
LAYER_CANVAS = 1024
LAYER_SCALE = 8.192
# The share of the canvas height the mark's grid takes on every full-bleed Apple surface.
MARK_SHARE = LAYER_SCALE * 100 / LAYER_CANVAS


def layer_artwork(path):
    transform = place(LAYER_CANVAS / 2, LAYER_CANVAS / 2, LAYER_SCALE)
    return svg(
        LAYER_CANVAS,
        LAYER_CANVAS,
        f'<path transform="{transform}" d="{path}" fill="#000000"/>',
    )


def full_bleed(width, height, palette, mark_height=None):
    """The background fill with the mark centered on it, as on an opaque square or banner."""
    scale = (mark_height or height * MARK_SHARE) / 100
    defs, body = mark(palette, place(width / 2, height / 2, scale))
    return svg(
        width,
        height,
        f'<rect width="{number(width)}" height="{number(height)}" fill="url(#bg)"/>' + body,
        palette.background.gradient("bg") + defs,
    )


def image_stack(width, height, palette):
    """Back, Middle and Front layers for a visionOS or tvOS image stack, with flat fills."""
    scale = height * MARK_SHARE / 100
    transform = place(width / 2, height / 2, scale)
    gradients = (
        palette.orbits.gradient("o") + palette.sun.gradient("s") + palette.planets.gradient("p")
    )
    return {
        "Back": svg(
            width,
            height,
            f'<rect width="{number(width)}" height="{number(height)}" fill="url(#bg)"/>',
            palette.background.gradient("bg"),
        ),
        "Middle": svg(
            width,
            height,
            f'<path transform="{transform}" d="{orbits_path()}" fill="url(#o)"/>',
            gradients,
        ),
        "Front": svg(
            width,
            height,
            f'<g transform="{transform}"><path d="{SUN}" fill="url(#s)"/>'
            f'<path d="{PLANETS}" fill="url(#p)"/></g>',
            gradients,
        ),
    }


# The tile that carries the mark outside Apple platforms, on a 128-unit canvas: Flathub's square
# grid shape, a flat top surface, and a darker front profile along the bottom, as GNOME asks.
TILE_CANVAS = 128
TILE_ORIGIN = 12.5
TILE_SIZE = 103
TILE_RADIUS = 8
TILE_PROFILE = 4
TILE_MARK_SCALE = 0.78


@dataclass(frozen=True)
class Surface:
    top: str
    profile: str


LIGHT_SURFACE = Surface("#EEF2FB", "#A7B3D8")
DARK_SURFACE = Surface("#18204A", "#070A1C")


def tile_body(palette, surface, prefix="t"):
    face = TILE_SIZE - TILE_PROFILE
    defs, body = mark(
        palette,
        place(TILE_CANVAS / 2, TILE_ORIGIN + face / 2, TILE_MARK_SCALE),
        prefix,
    )
    shape = (
        f'<rect x="{TILE_ORIGIN}" y="{{y}}" width="{TILE_SIZE}" height="{{height}}" '
        f'rx="{TILE_RADIUS}" fill="{{color}}"/>'
    )
    return defs, (
        shape.format(y=TILE_ORIGIN, height=TILE_SIZE, color=surface.profile)
        + shape.format(y=TILE_ORIGIN, height=face, color=surface.top)
        + body
    )


def tile(palette, surface=LIGHT_SURFACE):
    defs, body = tile_body(palette, surface)
    return svg(TILE_CANVAS, TILE_CANVAS, body, defs)


def favicon(light, dark):
    """The tile, redrawn with the dark palette when the browser prefers a dark scheme."""
    light_defs, light_body = tile_body(light, LIGHT_SURFACE, "l")
    dark_defs, dark_body = tile_body(dark, DARK_SURFACE, "d")
    style = (
        "<style>.d{display:none}@media (prefers-color-scheme:dark)"
        "{.l{display:none}.d{display:inline}}</style>"
    )
    return svg(
        TILE_CANVAS,
        TILE_CANVAS,
        f'{style}<g class="l">{light_body}</g><g class="d">{dark_body}</g>',
        light_defs + dark_defs,
    )


def glyph(size, color, badge=False):
    return svg(
        size,
        size,
        f'<path d="{glyph_path(badge)}" fill="{color}"/>',
        view_box="0 0 100 100",
    )


# Android's adaptive icon: a 108 dp layer whose content stays inside a 66 dp circle.
ADAPTIVE_CANVAS = 108
ADAPTIVE_SAFE_RADIUS = 33


def mark_extent():
    """The farthest any part of the mark reaches from the grid's center."""
    planet_reach = max(math.dist(CENTER, planet) + radius for _, planet, radius in ORBITS)
    return max(planet_reach, ORBITS[-1][0] + ORBIT_STROKE / 2)


def safe_scale(safe_radius):
    return safe_radius / mark_extent()


def android_vector(path, scale, color):
    transform_x = ADAPTIVE_CANVAS / 2 - CENTER[0] * scale
    return (
        '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
        f'    android:width="{ADAPTIVE_CANVAS}dp" android:height="{ADAPTIVE_CANVAS}dp"\n'
        f'    android:viewportWidth="{ADAPTIVE_CANVAS}" android:viewportHeight="{ADAPTIVE_CANVAS}">\n'
        f'  <group android:translateX="{number(transform_x)}" android:translateY="{number(transform_x)}"\n'
        f'      android:scaleX="{number(scale)}" android:scaleY="{number(scale)}">\n'
        f'    <path android:fillColor="{color}" android:pathData="{path}"/>\n'
        "  </group>\n</vector>\n"
    )


def android_background(palette):
    return (
        '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
        '    xmlns:aapt="http://schemas.android.com/aapt"\n'
        f'    android:width="{ADAPTIVE_CANVAS}dp" android:height="{ADAPTIVE_CANVAS}dp"\n'
        f'    android:viewportWidth="{ADAPTIVE_CANVAS}" android:viewportHeight="{ADAPTIVE_CANVAS}">\n'
        f'  <path android:pathData="M0,0h{ADAPTIVE_CANVAS}v{ADAPTIVE_CANVAS}h-{ADAPTIVE_CANVAS}z">\n'
        '    <aapt:attr name="android:fillColor">\n'
        f'      <gradient android:type="linear" android:startX="0" android:startY="0"\n'
        f'          android:endX="0" android:endY="{ADAPTIVE_CANVAS}">\n'
        f'        <item android:offset="0" android:color="{palette.background.top}"/>\n'
        f'        <item android:offset="1" android:color="{palette.background.bottom}"/>\n'
        "      </gradient>\n    </aapt:attr>\n  </path>\n</vector>\n"
    )


ADAPTIVE_ICON = (
    '<?xml version="1.0" encoding="utf-8"?>\n'
    '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
    '  <background android:drawable="@drawable/ic_launcher_background"/>\n'
    '  <foreground android:drawable="@mipmap/ic_launcher_foreground"/>\n'
    '  <monochrome android:drawable="@drawable/ic_launcher_monochrome"/>\n'
    "</adaptive-icon>\n"
)


def adaptive_foreground(palette):
    scale = safe_scale(ADAPTIVE_SAFE_RADIUS)
    defs, body = mark(palette, place(ADAPTIVE_CANVAS / 2, ADAPTIVE_CANVAS / 2, scale))
    return svg(ADAPTIVE_CANVAS, ADAPTIVE_CANVAS, body, defs)


def masked(palette, shape):
    """The adaptive icon composed and cut to a circle or a rounded square, for launchers
    without adaptive icons."""
    scale = safe_scale(ADAPTIVE_SAFE_RADIUS)
    defs, body = mark(palette, place(ADAPTIVE_CANVAS / 2, ADAPTIVE_CANVAS / 2, scale))
    half = ADAPTIVE_CANVAS / 2
    # The legacy icon shows the adaptive icon's central 72 dp.
    inset = (ADAPTIVE_CANVAS - 72) / 2
    clip = (
        f'<circle cx="{half}" cy="{half}" r="36"/>'
        if shape == "circle"
        else f'<rect x="{inset}" y="{inset}" width="72" height="72" rx="13"/>'
    )
    return svg(
        72,
        72,
        f'<g clip-path="url(#c)"><rect width="{ADAPTIVE_CANVAS}" height="{ADAPTIVE_CANVAS}" '
        f'fill="url(#bg)"/>{body}</g>',
        palette.background.gradient("bg") + f'<clipPath id="c">{clip}</clipPath>' + defs,
        view_box=f"{number(inset)} {number(inset)} 72 72",
    )


def maskable(palette, size):
    """A full-bleed web app icon whose mark stays inside the central 80 % circle."""
    defs, body = mark(palette, place(size / 2, size / 2, safe_scale(size * 0.4)))
    return svg(
        size,
        size,
        f'<rect width="{size}" height="{size}" fill="url(#bg)"/>' + body,
        palette.background.gradient("bg") + defs,
    )


def plated(palette, width, height, mark_share):
    """The tile centered on a transparent canvas, for Windows tiles and splash screens."""
    side = height * mark_share
    defs, body = tile_body(palette, LIGHT_SURFACE)
    scale = side / TILE_CANVAS
    return svg(
        width,
        height,
        f'<g transform="translate({number((width - side) / 2)} {number((height - side) / 2)}) '
        f'scale({number(scale)})">{body}</g>',
        defs,
    )


# The disk image window in points: its size, the icon size, and the centers of the app and the
# Applications link. Packaging lays the window out with these, so the art lines up with the icons.
DMG_WINDOW = (600, 400)
DMG_ICON_SIZE = 128
DMG_APP = (150, 170)
DMG_APPLICATIONS = (450, 170)
# Each orbit encloses its icon and the label Finder draws below it, inside the window.
DMG_ORBIT_RADIUS = 104
DMG_ORBIT_DROP = 16
DMG_ARROW_LENGTH = 56


def dmg_background(palette):
    """One faint orbit around each icon and its label, and an arrow toward Applications."""
    (width, height), app, applications = DMG_WINDOW, DMG_APP, DMG_APPLICATIONS
    # Finder draws the label below the icon, so each orbit's center sits below the icon's.
    rings = "".join(
        f'<circle cx="{x}" cy="{y + DMG_ORBIT_DROP}" r="{DMG_ORBIT_RADIUS}" fill="none" '
        f'stroke="{palette.orbits.bottom}" stroke-width="{ORBIT_STROKE * 1.2}" opacity="0.16"/>'
        for x, y in (app, applications)
    )
    middle = (app[0] + applications[0]) / 2
    half = DMG_ARROW_LENGTH / 2
    arrow = (
        f'<path d="M{middle - half} {app[1]}h{DMG_ARROW_LENGTH}m-18-18l18 18l-18 18" '
        f'fill="none" stroke="{palette.orbits.bottom}" stroke-width="7" '
        'stroke-linecap="round" stroke-linejoin="round"/>'
    )
    return svg(
        width,
        height,
        f'<rect width="{width}" height="{height}" fill="url(#bg)"/>{rings}{arrow}',
        palette.background.gradient("bg"),
    )


# ------------------------------------------------------------------------------------- wordmark
def outlined_text(font_path, text, size, x, baseline, color):
    """`text` as outlined paths, so the image never depends on installed fonts."""
    from fontTools.pens.svgPathPen import SVGPathPen
    from fontTools.pens.transformPen import TransformPen
    from fontTools.ttLib import TTFont

    font = TTFont(font_path)
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    scale = size / font["head"].unitsPerEm
    paths = []
    cursor = x
    for character in text:
        name = cmap[ord(character)]
        pen = SVGPathPen(glyphs)
        glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, cursor, baseline)))
        paths.append(pen.getCommands())
        cursor += glyphs[name].width * scale
    return f'<path d="{"".join(paths)}" fill="{color}"/>', cursor - x


def social_preview(root, palettes):
    """GitHub's 1280x640 link card: the tile, the name and what SpaceTerm is, on the dark fill."""
    light, dark = palettes
    width, height = 1280, 640
    defs, body = tile_body(light, LIGHT_SURFACE)
    side = 360
    name, _ = outlined_text(root / WORDMARK_FONT, "SpaceTerm", 112, 540, 318, "#FFFFFF")
    tagline, _ = outlined_text(
        root / TAGLINE_FONT,
        "A terminal multiplexer for macOS and Linux",
        34,
        544,
        386,
        dark.orbits.top,
    )
    return svg(
        width,
        height,
        f'<rect width="{width}" height="{height}" fill="url(#bg)"/>'
        f'<g transform="translate(120 {(height - side) / 2}) scale({side / TILE_CANVAS})">{body}</g>'
        + name
        + tagline,
        defs + Fill("#121A40", "#05060D").gradient("bg"),
    )


# ------------------------------------------------------------------------------------- encoders
def ico(images):
    """An ICO file holding each (size, PNG bytes) entry, as Windows Vista and later read."""
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries, data = b"", b""
    for size, png in images:
        dimension = 0 if size >= 256 else size
        entries += struct.pack(
            "<BBBBHHII", dimension, dimension, 0, 0, 1, 32, len(png), offset + len(data)
        )
        data += png
    return header + entries + data


# ---------------------------------------------------------------------------------------- tools
@dataclass(frozen=True)
class Renderers:
    """The external programs generation drives, injected so the plan can run without them."""

    rasterize: Callable[[str, int, int], bytes]
    icon_composer: Callable[[Path, str, str, int], bytes]
    hidpi_tiff: Callable[[bytes, bytes], bytes]
    outline: Callable[[Path, tuple[Palette, Palette]], str]


def run_tool(arguments, description):
    result = subprocess.run(arguments, capture_output=True)
    if result.returncode:
        raise TaskError(f"{description} failed")
    return result


def resvg(svg_text, width, height):
    with tempfile.TemporaryDirectory() as directory:
        source, output = Path(directory) / "in.svg", Path(directory) / "out.png"
        source.write_text(svg_text, encoding="utf-8")
        run_tool(
            ["resvg", "-w", str(width), "-h", str(height), str(source), str(output)],
            "resvg",
        )
        return output.read_bytes()


def ictool(document, platform, rendition, size):
    if not ICTOOL.is_file():
        raise TaskError("Icon Composer from Xcode 27 or newer is required")
    with tempfile.TemporaryDirectory() as directory:
        output = Path(directory) / "out.png"
        run_tool(
            [
                str(ICTOOL),
                str(document),
                "--export-image",
                "--output-file",
                str(output),
                "--platform",
                platform,
                "--rendition",
                rendition,
                "--width",
                str(size),
                "--height",
                str(size),
                "--scale",
                "1",
            ],
            "ictool",
        )
        return output.read_bytes()


def tiffutil(standard, retina):
    with tempfile.TemporaryDirectory() as directory:
        one, two, output = (Path(directory) / name for name in ("a.png", "a@2x.png", "a.tiff"))
        one.write_bytes(standard)
        two.write_bytes(retina)
        run_tool(
            ["tiffutil", "-cathidpicheck", str(one), str(two), "-out", str(output)], "tiffutil"
        )
        return output.read_bytes()


SYSTEM_RENDERERS = Renderers(resvg, ictool, tiffutil, social_preview)


# ----------------------------------------------------------------------------------------- plan
APPEARANCES = ("Default", "Dark", "ClearLight", "ClearDark", "TintedLight", "TintedDark")
HICOLOR_SIZES = (16, 22, 24, 32, 48, 64, 128, 256, 512)


def hicolor_index():
    """The hicolor theme index for exactly the folders generation writes."""
    sections = [(f"{size}x{size}/apps", f"Size={size}\nType=Fixed") for size in HICOLOR_SIZES] + [
        ("scalable/apps", "Size=128\nMinSize=8\nMaxSize=512\nType=Scalable"),
        ("symbolic/apps", "Size=16\nMinSize=8\nMaxSize=512\nType=Scalable"),
    ]
    header = (
        "[Icon Theme]\nName=Hicolor\nComment=Fallback icon theme\nHidden=true\n"
        f"Directories={','.join(folder for folder, _ in sections)}\n"
    )
    return header + "".join(
        f"\n[{folder}]\n{body}\nContext=Applications\n" for folder, body in sections
    )


ANDROID_DENSITIES = (("mdpi", 1), ("hdpi", 1.5), ("xhdpi", 2), ("xxhdpi", 3), ("xxxhdpi", 4))
WINDOWS_ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 256)
FAVICON_SIZES = (16, 32, 48)


def outputs(root, renderers):
    """Every generated file under assets/brand, keyed by its path relative to assets/brand."""
    document = json.loads((root / DOCUMENT / "icon.json").read_text(encoding="utf-8"))
    light, dark = palettes(document)
    files = {}

    def text(path, value):
        files[path] = value.encode("utf-8")

    def png(path, drawing, width, height=None):
        files[path] = renderers.rasterize(drawing, width, height or width)

    # Apple: the icon document's layer artwork, then renders of the finished document.
    text("apple/SpaceTerm.icon/Assets/Orbits.svg", layer_artwork(orbits_path()))
    text("apple/SpaceTerm.icon/Assets/Sun.svg", layer_artwork(SUN))
    text("apple/SpaceTerm.icon/Assets/Planets.svg", layer_artwork(PLANETS))
    with tempfile.TemporaryDirectory() as directory:
        staged = Path(directory) / "SpaceTerm.icon"
        shutil.copytree(root / DOCUMENT, staged)
        (staged / "Assets").mkdir(exist_ok=True)
        for name in ("Orbits", "Sun", "Planets"):
            path = f"apple/SpaceTerm.icon/Assets/{name}.svg"
            (staged / "Assets" / f"{name}.svg").write_bytes(files[path])
        for platform, folder in (("macOS", "macos"), ("iOS", "ios")):
            for rendition in APPEARANCES:
                files[f"{folder}/appearances/{rendition}.png"] = renderers.icon_composer(
                    staged, platform, rendition, 1024
                )
        # watchOS draws one appearance at its 1088-pixel layout size.
        files["watchos/AppIcon-1088.png"] = renderers.icon_composer(
            staged, "watchOS", "Default", 1088
        )

    # macOS menu bar: 16-point template glyphs on the 24-point menu bar. AppKit tints them.
    text("macos/menu-bar/SpaceTermTemplate.svg", glyph(16, "#000000"))
    text("macos/menu-bar/SpaceTermAttentionTemplate.svg", glyph(16, "#000000", badge=True))
    # The disk image window, at 1x and 2x in one TIFF.
    background = dmg_background(light)
    width, height = DMG_WINDOW
    files["macos/dmg/background.tiff"] = renderers.hidpi_tiff(
        renderers.rasterize(background, width, height),
        renderers.rasterize(background, width * 2, height * 2),
    )

    # visionOS and tvOS: layers Icon Composer cannot build, with the document's light colors.
    for name, drawing in image_stack(1024, 1024, light).items():
        png(f"visionos/layers-1024/{name}.png", drawing, 1024)
    for name, drawing in image_stack(400, 240, light).items():
        png(f"tvos/app-icon-layers-400x240/{name}.png", drawing, 400, 240)
        png(f"tvos/app-icon-layers-400x240/{name}@2x.png", drawing, 800, 480)
    for name, drawing in image_stack(1280, 768, light).items():
        png(f"tvos/app-store-layers-1280x768/{name}.png", drawing, 1280, 768)
    for name, width in (("TopShelf", 1920), ("TopShelfWide", 2320)):
        drawing = full_bleed(width, 720, light, mark_height=360)
        png(f"tvos/top-shelf/{name}.png", drawing, width, 720)
        png(f"tvos/top-shelf/{name}@2x.png", drawing, width * 2, 1440)

    # Linux: hicolor icons for the application ID, and the symbolic icon for small sizes and the
    # status area. The index makes the folder an icon theme, which status-area hosts need to look
    # up a name inside an installation's own icon directory.
    text("linux/hicolor/index.theme", hicolor_index())
    scalable = tile(light)
    text(f"linux/hicolor/scalable/apps/{APPLICATION_ID}.svg", scalable)
    for size in HICOLOR_SIZES:
        png(f"linux/hicolor/{size}x{size}/apps/{APPLICATION_ID}.png", scalable, size)
    text(f"linux/hicolor/symbolic/apps/{APPLICATION_ID}-symbolic.svg", glyph(16, "#2e3436"))
    text(
        f"linux/hicolor/symbolic/apps/{APPLICATION_ID}-attention-symbolic.svg",
        glyph(16, "#2e3436", badge=True),
    )

    # Windows: the application icon, and the MSIX package logos.
    files["windows/SpaceTerm.ico"] = ico(
        [(size, renderers.rasterize(scalable, size, size)) for size in WINDOWS_ICO_SIZES]
    )
    for size in (16, 24, 32, 48, 256):
        png(
            f"windows/msix/Square44x44Logo.targetsize-{size}_altform-unplated.png",
            scalable,
            size,
        )
    for name, width, height, share in (
        ("Square44x44Logo", 44, 44, 1.0),
        ("Square71x71Logo", 71, 71, 0.75),
        ("Square150x150Logo", 150, 150, 0.6),
        ("Wide310x150Logo", 310, 150, 0.6),
        ("Square310x310Logo", 310, 310, 0.6),
        ("StoreLogo", 50, 50, 1.0),
        ("SplashScreen", 620, 300, 0.5),
    ):
        drawing = plated(light, width, height, share)
        for scale in (100, 200):
            factor = scale / 100
            png(
                f"windows/msix/{name}.scale-{scale}.png",
                drawing,
                round(width * factor),
                round(height * factor),
            )

    # Android: the adaptive and themed launcher icon, older launchers' icons, and Google Play's.
    text("android/res/mipmap-anydpi-v26/ic_launcher.xml", ADAPTIVE_ICON)
    text("android/res/mipmap-anydpi-v26/ic_launcher_round.xml", ADAPTIVE_ICON)
    text("android/res/drawable/ic_launcher_background.xml", android_background(light))
    text(
        "android/res/drawable/ic_launcher_monochrome.xml",
        android_vector(glyph_path(), safe_scale(ADAPTIVE_SAFE_RADIUS), "#FFFFFFFF"),
    )
    foreground = adaptive_foreground(light)
    for density, factor in ANDROID_DENSITIES:
        folder = f"android/res/mipmap-{density}"
        png(f"{folder}/ic_launcher_foreground.png", foreground, round(108 * factor))
        png(f"{folder}/ic_launcher.png", masked(light, "square"), round(48 * factor))
        png(f"{folder}/ic_launcher_round.png", masked(light, "circle"), round(48 * factor))
    png("android/play-store-512.png", full_bleed(512, 512, light, mark_height=512 * 0.62), 512)

    # Web: a favicon that follows the color scheme, the touch icon, and web app icons.
    text("web/favicon.svg", favicon(light, dark))
    files["web/favicon.ico"] = ico(
        [(size, renderers.rasterize(scalable, size, size)) for size in FAVICON_SIZES]
    )
    png("web/apple-touch-icon.png", full_bleed(180, 180, light), 180)
    png("web/icon-192.png", scalable, 192)
    png("web/icon-512.png", scalable, 512)
    png("web/icon-maskable-512.png", maskable(light, 512), 512)

    # Logos for documents, sites and the README.
    text("logo/spaceterm-glyph.svg", glyph(100, "currentColor"))
    for name, palette in (("spaceterm-mark", light), ("spaceterm-mark-on-dark", dark)):
        defs, body = mark(palette, place(50, 50, 1))
        text(f"logo/{name}.svg", svg(100, 100, body, defs))
    text("logo/spaceterm-icon.svg", scalable)
    text("logo/spaceterm-icon-dark.svg", tile(dark, DARK_SURFACE))

    # GitHub's repository link card.
    png("github/social-preview.png", renderers.outline(root, (light, dark)), 1280, 640)
    return files


# ------------------------------------------------------------------------------------- manifest
GENERATOR = Path("mise-tasks/lib/spaceterm_tasks/brand.py")
SOURCES = (DOCUMENT / "icon.json", GENERATOR, WORDMARK_FONT, TAGLINE_FONT)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def manifest(root, files):
    return {
        "sources": {str(path): digest((root / path).read_bytes()) for path in SOURCES},
        "outputs": {path: digest(data) for path, data in sorted(files.items())},
    }


def generate(root=ROOT, renderers=SYSTEM_RENDERERS):
    """Write every generated brand file, remove ones no longer generated, and the manifest."""
    brand = root / BRAND
    files = outputs(root, renderers)
    previous = read_manifest(root).get("outputs", {}) if (root / MANIFEST).is_file() else {}
    for path in sorted(set(previous) - set(files)):
        (brand / path).unlink(missing_ok=True)
    for path, data in files.items():
        target = brand / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    for directory in sorted(brand.rglob("*"), reverse=True):
        if directory.is_dir() and not any(directory.iterdir()):
            directory.rmdir()
    (root / MANIFEST).write_text(
        json.dumps(manifest(root, files), indent=2) + "\n", encoding="utf-8"
    )
    print(f"Generated {len(files)} brand files.")


def read_manifest(root):
    try:
        return json.loads((root / MANIFEST).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise TaskError("assets/brand/manifest.json is missing or unreadable") from error


def check(root=ROOT):
    """Fail when the brand files no longer match the sources they were generated from."""
    recorded = read_manifest(root)
    brand = root / BRAND
    problems = []
    for path, expected in recorded.get("sources", {}).items():
        source = root / path
        if not source.is_file() or digest(source.read_bytes()) != expected:
            problems.append(f"{path} changed since the brand files were generated")
    outputs_recorded = recorded.get("outputs", {})
    for path, expected in outputs_recorded.items():
        output = brand / path
        if not output.is_file() or digest(output.read_bytes()) != expected:
            problems.append(f"assets/brand/{path} differs from what generation wrote")
    handwritten = {str(path.relative_to(BRAND)) for path in HANDWRITTEN} | {
        str(MANIFEST.relative_to(BRAND))
    }
    for path in sorted(brand.rglob("*")):
        relative = str(path.relative_to(brand))
        if path.is_file() and relative not in outputs_recorded and relative not in handwritten:
            problems.append(f"assets/brand/{relative} is neither generated nor a source")
    if problems:
        for problem in problems:
            print(problem)
        raise TaskError("the brand files are stale; run mise run brand on macOS")
    print("Brand files match their sources.")
