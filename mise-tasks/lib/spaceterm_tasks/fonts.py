"""Rename bundled font identities while preserving every glyph and metric table.

Requires fonttools; the fonts tasks provide it through spaceterm_tasks.venv.
"""

import hashlib
import io
import tarfile
import zipfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

# Family, unique, full, PostScript, typographic family, compatible full, WWS family, and
# variations PostScript name prefix records.
RENAMED_NAME_IDS = (1, 3, 4, 6, 16, 18, 21, 25)
# Unique and PostScript identities cannot contain family-name spaces.
COMPACT_NAME_IDS = (3, 6, 25)


@dataclass(frozen=True)
class FontSet:
    description: str
    archive_sha256: str
    destination: Path
    # Maps (name ID, original value) to the private value.
    rename: Callable[[int, str], str]
    # Reads the archive and returns {output file name: font or license bytes}.
    extract: Callable[[bytes], dict[str, bytes]]


def rename_terminal(_name_id, value):
    for old, new in (
        ("JetBrainsMono Nerd Font", "SpaceTerm Default"),
        ("JetBrainsMono NF", "SpaceTerm Default"),
        ("JetBrainsMonoNF", "SpaceTermDefault"),
    ):
        value = value.replace(old, new)
    return value


def rename_ui(name_id, value):
    return value.replace("Inter", "SpaceTermUI" if name_id in COMPACT_NAME_IDS else "SpaceTerm UI")


def extract_terminal(source):
    with tarfile.open(fileobj=io.BytesIO(source), mode="r:xz") as archive:
        members = {Path(member.name).name: member for member in archive.getmembers()}

        def read(name):
            file = archive.extractfile(members[name])
            if file is None:
                raise TaskError(f"the archive entry is not a file: {name}")
            return file.read()

        return {
            name: read(name)
            for name in (
                f"JetBrainsMonoNerdFont-{style}.ttf"
                for style in ("Regular", "Bold", "Italic", "BoldItalic")
            )
        }


def extract_ui(source):
    with zipfile.ZipFile(io.BytesIO(source)) as archive:
        fonts = {
            name: archive.read(f"extras/otf/{name}")
            for name in (
                f"Inter-{style}.otf" for style in ("Regular", "Medium", "SemiBold", "Bold")
            )
        }
        return {**fonts, "OFL.txt": archive.read("LICENSE.txt")}


TERMINAL = FontSet(
    "the unmodified Nerd Fonts 3.5.1 JetBrainsMono.tar.xz",
    "04d5e8f903693f9dd13e16f867e994834e681eb3c72c0d337a770dcda09010cf",
    ROOT / "assets/fonts/jetbrains-mono",
    rename_terminal,
    extract_terminal,
)
UI = FontSet(
    "the unmodified official Inter 4.1 Inter-4.1.zip",
    "9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e",
    ROOT / "assets/fonts/inter",
    rename_ui,
    extract_ui,
)


def private_font(source, rename):
    from fontTools.ttLib import TTFont

    font = TTFont(io.BytesIO(source), recalcTimestamp=False)
    original_tables = {tag: font.getTableData(tag) for tag in font.reader.keys()}
    for name in font["name"].names:
        if name.nameID in RENAMED_NAME_IDS:
            name.string = rename(name.nameID, name.toUnicode()).encode(name.getEncoding())
    output = io.BytesIO()
    font.save(output, reorderTables=False)
    result = output.getvalue()
    prepared = TTFont(io.BytesIO(result), recalcTimestamp=False)
    if set(prepared.reader.keys()) != set(original_tables):
        raise TaskError("font preparation changed the table inventory")
    for tag, original in original_tables.items():
        actual = prepared.getTableData(tag)
        if tag == "name":
            continue
        if tag == "head":
            # The whole-font checksum changes when the name table changes.
            original = original[:8] + original[12:]
            actual = actual[:8] + actual[12:]
        if actual != original:
            raise TaskError(f"font preparation changed the {tag} table")
    return result


def prepare(fonts, archive, check=False):
    """Write, or with check verify, the committed private fonts and their checksums."""
    source = Path(archive).read_bytes()
    if hashlib.sha256(source).hexdigest() != fonts.archive_sha256:
        raise TaskError(f"expected {fonts.description}")
    outputs = {
        name: data if not name.endswith((".ttf", ".otf")) else private_font(data, fonts.rename)
        for name, data in fonts.extract(source).items()
    }
    outputs["SHA256SUMS"] = "".join(
        f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, data in sorted(outputs.items())
    ).encode()
    if not check:
        fonts.destination.mkdir(parents=True, exist_ok=True)
    for name, data in outputs.items():
        path = fonts.destination / name
        if check:
            if not path.exists() or path.read_bytes() != data:
                raise TaskError(f"committed font artifact differs: {name}")
        else:
            path.write_bytes(data)
    print("Bundled font identities and unchanged glyph and metric tables verified.")
