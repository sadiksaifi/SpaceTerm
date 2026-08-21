#!/usr/bin/python3
"""Fail-closed validation of an archived render-performance trace bundle."""

from __future__ import annotations

import argparse
from decimal import Decimal, InvalidOperation
import hashlib
import os
from pathlib import Path, PurePosixPath
import stat
import subprocess
import sys
import zipfile


FORMAT_VERSION = 1
DEFAULT_MAXIMUM_ARCHIVE_BYTES = 20 * 1024 * 1024 * 1024
DEFAULT_MAXIMUM_UNCOMPRESSED_BYTES = 20 * 1024 * 1024 * 1024
DEFAULT_MAXIMUM_COMPRESSION_RATIO = 100
DEFAULT_MAXIMUM_MEMBER_COMPRESSION_RATIO = 1000
DEFAULT_MAXIMUM_MEMBERS = 250_000
DEFAULT_EXTRACTION_SAFETY_RESERVE_BYTES = 2 * 1024 * 1024 * 1024
COPY_CHUNK_BYTES = 1024 * 1024

TIME_PROFILE_XPATH = '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]'
ALLOCATIONS_XPATH = (
    '/trace-toc/run[@number="1"]/tracks/track[@name="Allocations"]'
    '/details/detail[@name="Allocations List"]'
)
HANGS_XPATH = '/trace-toc/run[@number="1"]/data/table[@schema="potential-hangs"]'


class EvidenceError(Exception):
    """A stable, user-facing fail-closed reason."""


def verdict(result: str, reason: str, **metrics: object) -> None:
    print(f"format_version\t{FORMAT_VERSION}")
    print(f"result\t{result}")
    print(f"reason\t{reason}")
    for key, value in metrics.items():
        print(f"{key}\t{value}")


def fail(reason: str) -> None:
    raise EvidenceError(reason)


def positive_integer(value: str) -> int:
    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def positive_decimal_text(value: str) -> str:
    try:
        parsed = Decimal(value)
    except InvalidOperation as error:
        raise argparse.ArgumentTypeError("must be a decimal") from error
    if not parsed.is_finite() or parsed <= 0:
        raise argparse.ArgumentTypeError("must be a positive finite decimal")
    return value


def regular_input(path: Path, label: str, *, allow_empty: bool = False) -> None:
    try:
        metadata = path.lstat()
    except OSError:
        fail(f"missing-{label}")
    if not stat.S_ISREG(metadata.st_mode) or (metadata.st_size == 0 and not allow_empty):
        fail(f"missing-or-invalid-{label}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(COPY_CHUNK_BYTES):
            digest.update(block)
    return digest.hexdigest()


def files_equal(left: Path, right: Path) -> bool:
    try:
        if left.stat().st_size != right.stat().st_size:
            return False
    except OSError:
        return False
    return sha256(left) == sha256(right)


def normalized_member_parts(name: str) -> tuple[str, ...]:
    if not name or "\\" in name or "\x00" in name:
        fail("trace-archive-member-path-unsafe")
    path = PurePosixPath(name)
    if path.is_absolute():
        fail("trace-archive-member-path-unsafe")
    raw_parts = name.rstrip("/").split("/")
    if not raw_parts or any(part in {"", ".", ".."} for part in raw_parts):
        fail("trace-archive-member-path-unsafe")
    if any(any(ord(character) < 32 for character in part) for part in raw_parts):
        fail("trace-archive-member-path-unsafe")
    return tuple(raw_parts)


def member_kind(info: zipfile.ZipInfo) -> str:
    unix_mode = (info.external_attr >> 16) & 0xFFFF
    file_type = stat.S_IFMT(unix_mode)
    if file_type == stat.S_IFLNK:
        fail("trace-archive-member-is-symlink")
    if file_type not in {0, stat.S_IFREG, stat.S_IFDIR}:
        fail("trace-archive-member-is-special")
    if info.is_dir():
        if info.file_size != 0 or file_type == stat.S_IFREG:
            fail("trace-archive-directory-member-is-invalid")
        return "directory"
    if file_type == stat.S_IFDIR:
        fail("trace-archive-file-member-is-invalid")
    return "file"


def preflight(
    archive: zipfile.ZipFile,
    *,
    maximum_uncompressed_bytes: int,
    maximum_compression_ratio: int,
    maximum_member_compression_ratio: int,
    maximum_members: int,
) -> tuple[str, int, int, int, list[tuple[zipfile.ZipInfo, tuple[str, ...], str]]]:
    members = archive.infolist()
    if not members or len(members) > maximum_members:
        fail("trace-archive-member-count-out-of-bounds")

    seen_names: set[str] = set()
    root_name: str | None = None
    total_uncompressed = 0
    total_compressed = 0
    checked: list[tuple[zipfile.ZipInfo, tuple[str, ...], str]] = []
    allowed_compressions = {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED}

    for info in members:
        parts = normalized_member_parts(info.filename)
        canonical_name = "/".join(parts)
        if canonical_name in seen_names:
            fail("trace-archive-has-duplicate-members")
        seen_names.add(canonical_name)

        if root_name is None:
            root_name = parts[0]
            if not root_name.endswith(".trace") or root_name == ".trace":
                fail("trace-archive-does-not-have-one-trace-root")
        if parts[0] != root_name:
            fail("trace-archive-has-sibling-members")
        if any(part.endswith(".trace") for part in parts[1:]):
            fail("trace-archive-has-nested-trace-root")
        if info.flag_bits & 0x1:
            fail("trace-archive-has-encrypted-members")
        if info.compress_type not in allowed_compressions:
            fail("trace-archive-compression-method-is-not-allowed")

        kind = member_kind(info)
        total_uncompressed += info.file_size
        total_compressed += info.compress_size
        if total_uncompressed > maximum_uncompressed_bytes:
            fail("trace-archive-uncompressed-bytes-exceed-limit")
        if info.file_size > 0:
            if info.compress_size <= 0:
                fail("trace-archive-member-compression-ratio-exceeds-limit")
            if info.file_size > info.compress_size * maximum_member_compression_ratio:
                fail("trace-archive-member-compression-ratio-exceeds-limit")
        checked.append((info, parts, kind))

    assert root_name is not None
    if total_uncompressed <= 0:
        fail("trace-archive-has-no-data")
    if total_compressed <= 0 or total_uncompressed > total_compressed * maximum_compression_ratio:
        fail("trace-archive-compression-ratio-exceeds-limit")
    return root_name, len(members), total_compressed, total_uncompressed, checked


def require_extraction_headroom(
    destination_parent: Path,
    declared_uncompressed_bytes: int,
    regenerated_artifact_bytes: int,
    safety_reserve_bytes: int,
    available_bytes_for_testing: int | None,
) -> tuple[int, int]:
    required_bytes = (
        declared_uncompressed_bytes
        + regenerated_artifact_bytes
        + safety_reserve_bytes
    )
    if available_bytes_for_testing is not None:
        if os.environ.get("SPACETERM_RENDER_PROFILE_TEST_OVERRIDES") != "1":
            fail("trace-archive-test-disk-override-not-enabled")
        available_bytes = available_bytes_for_testing
    else:
        try:
            filesystem = os.statvfs(destination_parent)
            available_bytes = filesystem.f_bavail * filesystem.f_frsize
        except OSError:
            fail("trace-archive-extraction-filesystem-unavailable")
    if available_bytes < 0:
        fail("trace-archive-extraction-filesystem-unavailable")
    if available_bytes < required_bytes:
        fail("trace-archive-insufficient-extraction-disk-headroom")
    return available_bytes, required_bytes


def extract_checked(
    archive: zipfile.ZipFile,
    destination: Path,
    members: list[tuple[zipfile.ZipInfo, tuple[str, ...], str]],
    maximum_uncompressed_bytes: int,
) -> None:
    if destination.exists():
        fail("trace-archive-extraction-destination-exists")
    try:
        destination.mkdir(mode=0o700, parents=False)
    except OSError:
        fail("trace-archive-extraction-destination-unavailable")

    written_total = 0
    for info, parts, kind in members:
        target = destination.joinpath(*parts)
        try:
            target.relative_to(destination)
        except ValueError:
            fail("trace-archive-member-path-unsafe")
        if kind == "directory":
            target.mkdir(mode=0o700, parents=True, exist_ok=True)
            continue
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        written_for_member = 0
        try:
            with archive.open(info, "r") as source, target.open("xb") as output:
                while block := source.read(COPY_CHUNK_BYTES):
                    written_for_member += len(block)
                    written_total += len(block)
                    if written_for_member > info.file_size or written_total > maximum_uncompressed_bytes:
                        fail("trace-archive-extraction-size-exceeds-declared-bounds")
                    output.write(block)
        except EvidenceError:
            raise
        except (OSError, RuntimeError, zipfile.BadZipFile):
            fail("trace-archive-member-cannot-be-extracted")
        if written_for_member != info.file_size:
            fail("trace-archive-extracted-size-does-not-match-directory")


def validate_extracted_tree(
    destination: Path,
    root_name: str,
    expected_uncompressed_bytes: int,
) -> Path:
    try:
        top_level = list(destination.iterdir())
    except OSError:
        fail("trace-archive-extracted-tree-cannot-be-read")
    if len(top_level) != 1 or top_level[0].name != root_name:
        fail("trace-archive-extracted-tree-has-siblings")
    trace_bundle = top_level[0]
    try:
        root_metadata = trace_bundle.lstat()
    except OSError:
        fail("trace-archive-extracted-trace-root-missing")
    if not stat.S_ISDIR(root_metadata.st_mode) or trace_bundle.is_symlink():
        fail("trace-archive-extracted-trace-root-invalid")

    actual_bytes = 0
    nonempty_files = 0
    try:
        for path in trace_bundle.rglob("*"):
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                fail("trace-archive-extracted-tree-has-symlink")
            if stat.S_ISREG(metadata.st_mode):
                actual_bytes += metadata.st_size
                if metadata.st_size > 0:
                    nonempty_files += 1
            elif not stat.S_ISDIR(metadata.st_mode):
                fail("trace-archive-extracted-tree-has-special-file")
    except OSError:
        fail("trace-archive-extracted-tree-cannot-be-read")
    if actual_bytes != expected_uncompressed_bytes:
        fail("trace-archive-extracted-bytes-do-not-match-directory")
    if nonempty_files == 0:
        fail("trace-archive-extracted-trace-has-no-data")
    return trace_bundle


def run_command(command: list[str], timeout_seconds: int, failure_reason: str) -> bytes:
    try:
        completed = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=timeout_seconds,
        )
    except (OSError, subprocess.TimeoutExpired):
        fail(failure_reason)
    if completed.returncode != 0:
        fail(failure_reason)
    return completed.stdout


def xctrace_export(
    xcrun: str,
    trace_bundle: Path,
    output: Path,
    timeout_seconds: int,
    *,
    xpath: str | None = None,
) -> None:
    command = [xcrun, "xctrace", "export", "--input", str(trace_bundle)]
    if xpath is None:
        command.append("--toc")
    else:
        command.extend(["--xpath", xpath])
    command.extend(["--output", str(output)])
    run_command(command, timeout_seconds, "trace-bundle-export-failed")
    regular_input(output, "regenerated-trace-export")


def compare_generated(generated: Path, supplied: Path, reason: str) -> None:
    if not files_equal(generated, supplied):
        fail(reason)


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--output-directory", required=True, type=Path)
    parser.add_argument("--xcrun", default="xcrun")
    parser.add_argument("--toc", required=True, type=Path)
    parser.add_argument("--time-profile", required=True, type=Path)
    parser.add_argument("--allocations", required=True, type=Path)
    parser.add_argument("--hangs", required=True, type=Path)
    parser.add_argument("--trace-verifier", required=True, type=Path)
    parser.add_argument("--python", default="/usr/bin/python3")
    parser.add_argument("--verification", required=True, type=Path)
    parser.add_argument("--pid", required=True, type=positive_integer)
    parser.add_argument("--process-name", required=True)
    parser.add_argument("--requested-seconds", required=True, type=positive_integer)
    parser.add_argument(
        "--command-elapsed-seconds", required=True, type=positive_decimal_text
    )
    parser.add_argument(
        "--maximum-archive-bytes",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_ARCHIVE_BYTES,
    )
    parser.add_argument(
        "--maximum-uncompressed-bytes",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_UNCOMPRESSED_BYTES,
    )
    parser.add_argument(
        "--maximum-compression-ratio",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_COMPRESSION_RATIO,
    )
    parser.add_argument(
        "--maximum-member-compression-ratio",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_MEMBER_COMPRESSION_RATIO,
    )
    parser.add_argument(
        "--maximum-members",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_MEMBERS,
    )
    parser.add_argument(
        "--extraction-safety-reserve-bytes",
        type=positive_integer,
        default=DEFAULT_EXTRACTION_SAFETY_RESERVE_BYTES,
    )
    parser.add_argument(
        "--available-disk-bytes-for-testing",
        type=positive_integer,
        help=argparse.SUPPRESS,
    )
    parser.add_argument("--command-timeout-seconds", type=positive_integer, default=600)
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    try:
        for path, label in (
            (arguments.archive, "trace-archive"),
            (arguments.toc, "trace-toc"),
            (arguments.time_profile, "time-profile-export"),
            (arguments.allocations, "allocations-export"),
            (arguments.hangs, "hangs-export"),
            (arguments.trace_verifier, "trace-verifier"),
            (arguments.verification, "trace-verification"),
        ):
            regular_input(path, label)
        archive_bytes = arguments.archive.stat().st_size
        if archive_bytes > arguments.maximum_archive_bytes:
            fail("trace-archive-bytes-exceed-limit")
        if not arguments.process_name or any(
            character in arguments.process_name for character in "\t\r\n"
        ):
            fail("invalid-trace-process-name")
        try:
            regenerated_artifact_bytes = sum(
                path.stat().st_size
                for path in (
                    arguments.toc,
                    arguments.time_profile,
                    arguments.allocations,
                    arguments.hangs,
                    arguments.verification,
                )
            )
        except OSError:
            fail("trace-regenerated-artifact-size-unavailable")

        extraction_directory = arguments.output_directory / "extracted"
        generated_directory = arguments.output_directory / "generated"
        if arguments.output_directory.exists():
            fail("trace-validation-output-directory-exists")
        try:
            arguments.output_directory.mkdir(mode=0o700, parents=False)
            generated_directory.mkdir(mode=0o700)
        except OSError:
            fail("trace-validation-output-directory-unavailable")

        try:
            with zipfile.ZipFile(arguments.archive, "r") as archive:
                root, member_count, compressed, uncompressed, members = preflight(
                    archive,
                    maximum_uncompressed_bytes=arguments.maximum_uncompressed_bytes,
                    maximum_compression_ratio=arguments.maximum_compression_ratio,
                    maximum_member_compression_ratio=(
                        arguments.maximum_member_compression_ratio
                    ),
                    maximum_members=arguments.maximum_members,
                )
                available_disk_bytes, required_disk_bytes = require_extraction_headroom(
                    extraction_directory.parent,
                    uncompressed,
                    regenerated_artifact_bytes,
                    arguments.extraction_safety_reserve_bytes,
                    arguments.available_disk_bytes_for_testing,
                )
                extract_checked(
                    archive,
                    extraction_directory,
                    members,
                    arguments.maximum_uncompressed_bytes,
                )
        except EvidenceError:
            raise
        except (OSError, zipfile.BadZipFile, zipfile.LargeZipFile):
            fail("trace-archive-is-not-a-valid-zip")

        trace_bundle = validate_extracted_tree(extraction_directory, root, uncompressed)
        generated_toc = generated_directory / "toc.xml"
        generated_time = generated_directory / "time-profile.xml"
        generated_allocations = generated_directory / "allocations.xml"
        generated_hangs = generated_directory / "hangs.xml"
        generated_verification = generated_directory / "verification.tsv"

        xctrace_export(
            arguments.xcrun,
            trace_bundle,
            generated_toc,
            arguments.command_timeout_seconds,
        )
        compare_generated(
            generated_toc, arguments.toc, "trace-toc-does-not-match-archive"
        )
        for xpath, output, supplied, mismatch_reason in (
            (
                TIME_PROFILE_XPATH,
                generated_time,
                arguments.time_profile,
                "time-profile-export-does-not-match-archive",
            ),
            (
                ALLOCATIONS_XPATH,
                generated_allocations,
                arguments.allocations,
                "allocations-export-does-not-match-archive",
            ),
            (
                HANGS_XPATH,
                generated_hangs,
                arguments.hangs,
                "hangs-export-does-not-match-archive",
            ),
        ):
            xctrace_export(
                arguments.xcrun,
                trace_bundle,
                output,
                arguments.command_timeout_seconds,
                xpath=xpath,
            )
            compare_generated(output, supplied, mismatch_reason)

        verifier_stdout = run_command(
            [
                arguments.python,
                str(arguments.trace_verifier),
                "--toc",
                str(generated_toc),
                "--time-profile",
                str(generated_time),
                "--allocations",
                str(generated_allocations),
                "--hangs",
                str(generated_hangs),
                "--pid",
                str(arguments.pid),
                "--process-name",
                arguments.process_name,
                "--requested-seconds",
                str(arguments.requested_seconds),
                "--command-elapsed-seconds",
                arguments.command_elapsed_seconds,
            ],
            arguments.command_timeout_seconds,
            "trace-verifier-rejected-regenerated-exports",
        )
        try:
            generated_verification.write_bytes(verifier_stdout)
        except OSError:
            fail("trace-verification-receipt-cannot-be-written")
        compare_generated(
            generated_verification,
            arguments.verification,
            "trace-verification-receipt-does-not-match-regenerated-exports",
        )

        verdict(
            "PASS",
            "trace-archive-and-regenerated-exports-verified",
            archive_bytes=archive_bytes,
            member_count=member_count,
            compressed_bytes=compressed,
            uncompressed_bytes=uncompressed,
            regenerated_artifact_bytes=regenerated_artifact_bytes,
            extraction_available_bytes=available_disk_bytes,
            extraction_required_bytes=required_disk_bytes,
            toc_sha256=sha256(generated_toc),
            time_profile_sha256=sha256(generated_time),
            allocations_sha256=sha256(generated_allocations),
            hangs_sha256=sha256(generated_hangs),
            verification_sha256=sha256(generated_verification),
        )
        return 0
    except EvidenceError as error:
        verdict("NOT-RUN", str(error))
        return 2


if __name__ == "__main__":
    sys.exit(main())
