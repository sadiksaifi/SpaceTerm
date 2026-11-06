"""Decompress once to bounded private storage before tarfile interprets extension records."""

import gzip
import tarfile
import tempfile
from contextlib import contextmanager

from spaceterm_tasks import TaskError

EXTENSION_BYTES = 4096


def _pax(data):
    while data:
        prefix, separator, _ = data.partition(b" ")
        if not separator or not prefix.isdigit():
            raise TaskError("the archive extension is invalid")
        length = int(prefix)
        if length <= len(prefix) + 2 or length > len(data):
            raise TaskError("the archive extension is invalid")
        field = data[len(prefix) + 1 : length]
        if not field.endswith(b"\n") or field.partition(b"=")[0] not in (
            b"path",
            b"mtime",
            b"atime",
            b"ctime",
        ):
            raise TaskError("the archive extension is unsupported")
        data = data[length:]


@contextmanager
def open_archive(path, *, maximum, members):
    """Bound every decompressed byte and physical record, including GNU/PAX metadata."""
    with tempfile.TemporaryFile() as expanded, gzip.open(path, "rb") as source:
        total = 0
        records = 0
        entries = 0

        def copy(size):
            nonlocal total
            if size < 0 or total + size > maximum:
                raise TaskError("the archive expanded size exceeds its limit")
            remaining = size
            while remaining:
                block = source.read(min(remaining, 64 * 1024))
                if not block:
                    raise TaskError("the archive is truncated")
                total += len(block)
                expanded.write(block)
                remaining -= len(block)

        while True:
            header = source.read(min(512, maximum - total + 1))
            if not header:
                break
            total += len(header)
            if total > maximum or len(header) != 512:
                raise TaskError("the archive expanded size is invalid")
            expanded.write(header)
            if header == bytes(512):
                # Include tar padding and concatenated gzip streams in the same byte budget.
                while block := source.read(min(64 * 1024, maximum - total + 1)):
                    total += len(block)
                    if total > maximum:
                        raise TaskError("the archive expanded size exceeds its limit")
                    expanded.write(block)
                break
            records += 1
            if records > members * 2:
                raise TaskError("the archive has too many physical records")
            entry = tarfile.TarInfo.frombuf(header, "utf-8", "surrogateescape")
            if entry.size < 0:
                raise TaskError("the archive member size is invalid")
            extension = entry.type in (
                tarfile.GNUTYPE_LONGNAME,
                tarfile.GNUTYPE_LONGLINK,
                tarfile.XHDTYPE,
                tarfile.XGLTYPE,
            )
            if extension:
                if entry.size > EXTENSION_BYTES:
                    raise TaskError("the archive extension exceeds its limit")
                if total + (entry.size + 511) // 512 * 512 > maximum:
                    raise TaskError("the archive expanded size exceeds its limit")
                data = source.read(entry.size)
                if len(data) != entry.size:
                    raise TaskError("the archive extension is truncated")
                total += len(data)
                if total > maximum:
                    raise TaskError("the archive expanded size exceeds its limit")
                expanded.write(data)
                if entry.type in (tarfile.XHDTYPE, tarfile.XGLTYPE):
                    _pax(data)
                copy((-entry.size) % 512)
            elif entry.type in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE):
                entries += 1
                if entries > members:
                    raise TaskError("the archive has too many members")
                copy((entry.size + 511) // 512 * 512)
            else:
                raise TaskError("the archive member type is unsupported")
        expanded.seek(0)
        with tarfile.open(fileobj=expanded, mode="r:") as archive:
            yield archive
