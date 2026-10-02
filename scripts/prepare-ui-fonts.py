# /// script
# requires-python = ">=3.13"
# dependencies = ["fonttools==4.66.0"]
# ///
"""Prepare private UI font identities without changing glyphs or metrics."""

import argparse
import hashlib
import io
from pathlib import Path
import zipfile

from fontTools.ttLib import TTFont


ARCHIVE_SHA256 = "9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e"
STYLES = ("Regular", "Medium", "SemiBold", "Bold")
DESTINATION = Path(__file__).resolve().parent.parent / "assets/fonts/inter"


def private_font(source):
    font = TTFont(io.BytesIO(source), recalcTimestamp=False)
    original_tables = {tag: font.getTableData(tag) for tag in font.reader.keys()}
    for name in font["name"].names:
        if name.nameID in (1, 3, 4, 6, 16, 18, 21, 25):
            # Unique and PostScript identities cannot contain family-name spaces.
            family = "SpaceTermUI" if name.nameID in (3, 6, 25) else "SpaceTerm UI"
            name.string = name.toUnicode().replace("Inter", family).encode(name.getEncoding())
    output = io.BytesIO()
    font.save(output, reorderTables=False)
    result = output.getvalue()
    prepared = TTFont(io.BytesIO(result), recalcTimestamp=False)
    if set(prepared.reader.keys()) != set(original_tables):
        raise ValueError("Font preparation changed the table inventory")
    for tag, original in original_tables.items():
        actual = prepared.getTableData(tag)
        if tag == "name":
            continue
        if tag == "head":
            # The whole-font checksum changes when the name table changes.
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
        parser.error("expected the unmodified official Inter 4.1 Inter-4.1.zip")
    outputs = {}
    with zipfile.ZipFile(io.BytesIO(source)) as archive:
        for style in STYLES:
            name = f"Inter-{style}.otf"
            outputs[name] = private_font(archive.read(f"extras/otf/{name}"))
        outputs["OFL.txt"] = archive.read("LICENSE.txt")
    outputs["SHA256SUMS"] = "".join(
        f"{hashlib.sha256(data).hexdigest()}  {name}\n"
        for name, data in sorted(outputs.items())
    ).encode()
    if not args.check:
        DESTINATION.mkdir(parents=True, exist_ok=True)
    for name, data in outputs.items():
        path = DESTINATION / name
        if args.check:
            if not path.exists() or path.read_bytes() != data:
                parser.error(f"committed font artifact differs: {name}")
        else:
            path.write_bytes(data)
    print("Bundled UI font identities and unchanged glyph/metric tables verified.")


if __name__ == "__main__":
    main()
