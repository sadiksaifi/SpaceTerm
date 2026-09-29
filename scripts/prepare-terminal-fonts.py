# /// script
# requires-python = ">=3.13"
# dependencies = ["fonttools==4.66.0"]
# ///
"""Rename bundled font identities while preserving every glyph and metric table."""

import argparse
import hashlib
import io
from pathlib import Path
import tarfile

from fontTools.ttLib import TTFont


ARCHIVE_SHA256 = "04d5e8f903693f9dd13e16f867e994834e681eb3c72c0d337a770dcda09010cf"
STYLES = ("Regular", "Bold", "Italic", "BoldItalic")
DESTINATION = Path(__file__).resolve().parent.parent / "assets/fonts/jetbrains-mono"


def private_font(source):
    font = TTFont(io.BytesIO(source), recalcTimestamp=False)
    original_tables = {tag: font.getTableData(tag) for tag in font.reader.keys()}
    for name in font["name"].names:
        if name.nameID in (1, 3, 4, 6, 16, 18, 21, 25):
            value = name.toUnicode()
            for old, new in (
                ("JetBrainsMono Nerd Font", "SpaceTerm Default"),
                ("JetBrainsMono NF", "SpaceTerm Default"),
                ("JetBrainsMonoNF", "SpaceTermDefault"),
            ):
                value = value.replace(old, new)
            name.string = value.encode(name.getEncoding())
    output = io.BytesIO()
    font.save(output, reorderTables=False)
    result = output.getvalue()
    prepared = TTFont(io.BytesIO(result), recalcTimestamp=False)
    for tag, original in original_tables.items():
        actual = prepared.getTableData(tag)
        if tag == "name":
            continue
        if tag == "head":
            # The table checksum changes when the name table changes.
            original = original[:8] + original[12:]
            actual = actual[:8] + actual[12:]
        if actual != original:
            raise ValueError(f"Font preparation changed the {tag} table")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    source = args.archive.read_bytes()
    if hashlib.sha256(source).hexdigest() != ARCHIVE_SHA256:
        parser.error("expected the unmodified Nerd Fonts 3.5.1 JetBrainsMono.tar.xz")
    outputs = {}
    with tarfile.open(fileobj=io.BytesIO(source), mode="r:xz") as archive:
        members = {Path(member.name).name: member for member in archive.getmembers()}
        for style in STYLES:
            name = f"JetBrainsMonoNerdFont-{style}.ttf"
            outputs[name] = private_font(archive.extractfile(members[name]).read())
    outputs["SHA256SUMS"] = "".join(
        f"{hashlib.sha256(data).hexdigest()}  {name}\n"
        for name, data in sorted(outputs.items())
    ).encode()
    for name, data in outputs.items():
        path = DESTINATION / name
        if args.check:
            if not path.exists() or path.read_bytes() != data:
                parser.error(f"committed font artifact differs: {name}")
        else:
            path.write_bytes(data)
    print("Bundled font identities and unchanged glyph/metric tables verified.")


if __name__ == "__main__":
    main()
