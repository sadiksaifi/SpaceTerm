#!/usr/bin/python3
"""Create the canonical single-root ZIP consumed by render trace verification."""

from __future__ import annotations

import argparse
import os
import pathlib
import stat
import unicodedata
import zipfile
import hashlib


COPY_CHUNK_BYTES = 1024 * 1024


def stable_identity(value: os.stat_result) -> tuple[int, ...]:
    return (
        value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_nlink,
        value.st_size, value.st_mtime_ns, value.st_ctime_ns,
    )


def open_stable_regular(path: pathlib.Path, expected: tuple[int, ...]):
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    opened = os.fstat(descriptor)
    if not stat.S_ISREG(opened.st_mode) or stable_identity(opened) != expected:
        os.close(descriptor)
        raise SystemExit("error: trace entry changed while it was opened")
    return os.fdopen(descriptor, "rb")


def hash_stable_file(path: pathlib.Path,
                     expected: tuple[int, ...]) -> str:
    digest = hashlib.sha256()
    with open_stable_regular(path, expected) as source:
        for block in iter(lambda: source.read(COPY_CHUNK_BYTES), b""):
            digest.update(block)
        opened_after = os.fstat(source.fileno())
    path_after = path.lstat()
    if stable_identity(opened_after) != expected or stable_identity(path_after) != expected:
        raise SystemExit("error: trace entry changed while it was hashed")
    return digest.hexdigest()


def stream_stable_file(archive: zipfile.ZipFile, path: pathlib.Path,
                       archive_name: str,
                       fingerprint: tuple[object, ...]) -> None:
    expected = fingerprint[:-1]
    info = zipfile.ZipInfo.from_file(path, archive_name, strict_timestamps=False)
    info.compress_type = zipfile.ZIP_DEFLATED
    digest = hashlib.sha256()
    with open_stable_regular(path, expected) as source, archive.open(
        info, "w", force_zip64=True
    ) as destination:
        while block := source.read(COPY_CHUNK_BYTES):
            digest.update(block)
            destination.write(block)
        opened_after = os.fstat(source.fileno())
    path_after = path.lstat()
    observed = (*stable_identity(opened_after), digest.hexdigest())
    if (observed != fingerprint
            or stable_identity(path_after) != expected):
        raise SystemExit("error: trace entry changed during archive streaming")


def current_entries(trace: pathlib.Path) -> list[tuple[str, str]]:
    result: list[tuple[str, str]] = []
    for root, directories, files in os.walk(trace, followlinks=False):
        directories.sort(); files.sort()
        root_path = pathlib.Path(root)
        for name in directories + files:
            path = root_path / name
            details = path.lstat()
            if stat.S_ISLNK(details.st_mode) or not (
                    stat.S_ISDIR(details.st_mode) or stat.S_ISREG(details.st_mode)):
                raise SystemExit("error: trace contains symbolic or special entries")
            relative = path.relative_to(trace).as_posix()
            if unicodedata.normalize("NFC", relative) != relative:
                raise SystemExit("error: trace entry path is not canonical NFC")
            result.append((relative, "directory" if stat.S_ISDIR(details.st_mode) else "file"))
    return sorted(result)


def require_directories_unchanged(
    directories: dict[pathlib.Path, tuple[int, ...]],
) -> None:
    for path, expected in directories.items():
        if stable_identity(path.lstat()) != expected:
            raise SystemExit("error: trace directory changed during archive creation")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--trace", required=True)
    parser.add_argument("--output", required=True)
    arguments = parser.parse_args()
    trace = pathlib.Path(arguments.trace)
    output = pathlib.Path(arguments.output)
    if (not trace.is_absolute() or trace.is_symlink() or not trace.is_dir()
            or trace.name in ("", ".", "..") or not trace.name.endswith(".trace")):
        raise SystemExit("error: trace must be one canonical physical .trace directory")
    if trace.resolve(strict=True) != trace:
        raise SystemExit("error: trace path must be canonical")
    if not output.is_absolute() or output.exists() or output.is_symlink() or output.suffix != ".zip":
        raise SystemExit("error: output must be an absent absolute .zip path")
    parent = output.parent.resolve(strict=True)
    if output != parent / output.name or parent.is_symlink():
        raise SystemExit("error: output parent must be canonical and physical")
    entries: list[tuple[pathlib.Path, str, tuple[object, ...] | None]] = []
    observed_entries: list[tuple[str, str]] = []
    directories_before: dict[pathlib.Path, tuple[int, ...]] = {
        trace: stable_identity(trace.lstat())
    }
    total_size = 0
    for root, directories, files in os.walk(trace, followlinks=False):
        directories.sort()
        files.sort()
        root_path = pathlib.Path(root)
        for name in directories + files:
            path = root_path / name
            details = path.lstat()
            if stat.S_ISLNK(details.st_mode) or not (
                    stat.S_ISDIR(details.st_mode) or stat.S_ISREG(details.st_mode)):
                raise SystemExit("error: trace contains symbolic or special entries")
            relative = path.relative_to(trace)
            relative_text = relative.as_posix()
            if unicodedata.normalize("NFC", relative_text) != relative_text:
                raise SystemExit("error: trace entry path is not canonical NFC")
            archive_name = pathlib.PurePosixPath(trace.name, *relative.parts).as_posix()
            fingerprint = None
            if stat.S_ISDIR(details.st_mode):
                archive_name += "/"
                directories_before[path] = stable_identity(details)
                observed_entries.append((relative_text, "directory"))
            else:
                observed_entries.append((relative_text, "file"))
                total_size += details.st_size
                if details.st_size > 2 * 1024 * 1024 * 1024 or total_size > 4 * 1024 * 1024 * 1024:
                    raise SystemExit("error: trace archive exceeds the bounded size")
                expected = stable_identity(details)
                digest = hash_stable_file(path, expected)
                fingerprint = (*expected, digest)
            entries.append((path, archive_name, fingerprint))
    if sorted(observed_entries) != current_entries(trace):
        raise SystemExit("error: trace entry set changed during archive preflight")
    require_directories_unchanged(directories_before)
    if not entries or len(entries) > 200_000:
        raise SystemExit("error: trace archive entry count is invalid")
    filesystem = os.statvfs(parent)
    available = filesystem.f_bavail * filesystem.f_frsize
    required = total_size + max(64 * 1024 * 1024, total_size // 10)
    if available < required:
        raise SystemExit("error: insufficient output-parent space for trace archive")
    temporary = parent / f".{output.name}.{os.getpid()}.tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.close(descriptor)
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED,
                             compresslevel=6, allowZip64=True) as archive:
            for path, archive_name, fingerprint in entries:
                if archive_name.endswith("/"):
                    info = zipfile.ZipInfo(archive_name)
                    info.external_attr = (stat.S_IFDIR | 0o555) << 16
                    archive.writestr(info, b"")
                else:
                    before = path.lstat()
                    if fingerprint is None or fingerprint[:-1] != stable_identity(before):
                        raise SystemExit("error: trace entry changed before archive streaming")
                    stream_stable_file(archive, path, archive_name, fingerprint)
        if sorted(observed_entries) != current_entries(trace):
            raise SystemExit("error: trace entry set changed during archive streaming")
        require_directories_unchanged(directories_before)
        if temporary.stat().st_size > 4 * 1024 * 1024 * 1024:
            raise SystemExit("error: compressed trace archive exceeds the bounded size")
        os.chmod(temporary, 0o444)
        os.link(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
