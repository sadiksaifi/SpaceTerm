#!/usr/bin/env python3
"""Supervise an artifact-producing command under a bounded Cargo target budget."""

from __future__ import annotations

import argparse
import errno
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


BREACH_STATUS = 75
MONITOR_INTERVAL_SECONDS = 1.0
POLL_INTERVAL_SECONDS = 0.05
TERMINATE_GRACE_SECONDS = 1.0
KILL_GRACE_SECONDS = 5.0
FORWARDED_SIGNALS = (signal.SIGHUP, signal.SIGINT, signal.SIGTERM)


class Supervisor:
    def __init__(self, repo_dir: Path, target_dir: Path, budget_kib: int) -> None:
        self.repo_dir = repo_dir.resolve()
        self.target_dir = target_dir.resolve()
        self.budget_kib = budget_kib
        self.active_process: subprocess.Popen[bytes] | None = None
        self.active_pgid: int | None = None
        self.received_signal: int | None = None

    def install_signal_handlers(self) -> None:
        for signum in FORWARDED_SIGNALS:
            signal.signal(signum, self._handle_signal)

    def _handle_signal(self, signum: int, _frame: object) -> None:
        if self.received_signal is None:
            self.received_signal = signum
        self._signal_active_group(signum)

    def _signal_active_group(self, signum: int) -> None:
        if self.active_pgid is None:
            return
        try:
            os.killpg(self.active_pgid, signum)
        except ProcessLookupError:
            pass

    def _spawn_session(
        self, command: list[str], *, env: dict[str, str] | None = None
    ) -> subprocess.Popen[bytes]:
        previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, FORWARDED_SIGNALS)
        try:
            # The parent keeps forwarding signals blocked until it owns the new
            # process group. Restore the original mask in this single-threaded
            # forked child so the exec'd command receives signals normally.
            process = subprocess.Popen(
                command,
                env=env,
                start_new_session=True,
                preexec_fn=lambda: signal.pthread_sigmask(
                    signal.SIG_SETMASK, previous_mask
                ),
            )
            self.active_process = process
            self.active_pgid = process.pid
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
        return process

    def _clear_active(self) -> None:
        self.active_process = None
        self.active_pgid = None

    def _group_exists(self) -> bool:
        if self.active_pgid is None:
            return False
        try:
            os.killpg(self.active_pgid, 0)
        except ProcessLookupError:
            return False
        except PermissionError:
            return True
        return True

    def _group_has_live_processes(self) -> bool:
        """Return false when a group is absent or contains only harmless zombies."""
        if self.active_pgid is None:
            return False
        try:
            result = subprocess.run(
                ["ps", "-axo", "pgid=,stat="],
                check=False,
                capture_output=True,
                text=True,
            )
        except OSError:
            return self._group_exists()
        if result.returncode != 0:
            return self._group_exists()
        expected = str(self.active_pgid)
        for line in result.stdout.splitlines():
            fields = line.split()
            if len(fields) >= 2 and fields[0] == expected and not fields[1].startswith("Z"):
                return True
        return False

    def _wait_until_group_stops(self, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while self._group_has_live_processes():
            if time.monotonic() >= deadline:
                return False
            time.sleep(POLL_INTERVAL_SECONDS)
        return True

    def terminate_active_group(self) -> bool:
        process = self.active_process
        if process is None:
            return True
        self._signal_active_group(signal.SIGTERM)
        stopped = self._wait_until_group_stops(TERMINATE_GRACE_SECONDS)
        if not stopped:
            self._signal_active_group(signal.SIGKILL)
            stopped = self._wait_until_group_stops(KILL_GRACE_SECONDS)
        try:
            process.wait(timeout=0 if stopped else POLL_INTERVAL_SECONDS)
        except subprocess.TimeoutExpired:
            stopped = False
        return stopped

    def target_size_kib(self) -> int:
        if not self.target_dir.is_dir():
            return 0
        try:
            result = subprocess.run(
                ["du", "-sk", str(self.target_dir)],
                check=False,
                capture_output=True,
                text=True,
            )
            if self.received_signal is not None:
                return 0
            if result.returncode != 0:
                raise ValueError
            return int(result.stdout.split()[0])
        except (OSError, ValueError, IndexError):
            print("error: could not measure Cargo artifact usage", file=sys.stderr)
            raise SystemExit(2) from None

    def target_is_verified(self) -> bool:
        return self.target_dir == self.repo_dir / "target" or (
            self.target_dir / ".rustc_info.json"
        ).is_file()

    def clean_target(self) -> int:
        if not self.target_dir.is_dir():
            return 0
        if not self.target_is_verified():
            print("error: refusing to clean an unverified Cargo target directory", file=sys.stderr)
            return 2
        process = self._spawn_session(
            [
                "cargo",
                "clean",
                "--manifest-path",
                str(self.repo_dir / "Cargo.toml"),
                "--target-dir",
                str(self.target_dir),
            ]
        )
        status = process.wait()
        self._clear_active()
        if self.received_signal is not None:
            return 128 + self.received_signal
        return self.shell_status(status)

    @staticmethod
    def shell_status(status: int) -> int:
        return 128 - status if status < 0 else status

    def run(self, command: list[str]) -> int:
        if self.target_size_kib() > self.budget_kib:
            print(
                "Cargo target exceeds its disk budget from a previous command; cleaning it.",
                file=sys.stderr,
            )
            cleanup_status = self.clean_target()
            if cleanup_status != 0:
                return cleanup_status
        if self.received_signal is not None:
            return 128 + self.received_signal

        child_env = os.environ.copy()
        child_env["CARGO_INCREMENTAL"] = "0"
        child_env["SPACETERM_CARGO_ARTIFACT_GUARD_ACTIVE"] = "1"
        try:
            process = self._spawn_session(command, env=child_env)
        except OSError as error:
            if error.errno == errno.ENOENT:
                print("error: guarded command could not be started", file=sys.stderr)
                return 127
            print("error: guarded command could not be started", file=sys.stderr)
            return 2

        leader_status: int | None = None
        next_measurement = 0.0
        budget_breached = False
        termination_verified = True

        while True:
            if leader_status is None:
                leader_status = process.poll()

            if self.received_signal is not None:
                termination_verified = self.terminate_active_group()
                break

            now = time.monotonic()
            if now >= next_measurement:
                if self.target_size_kib() > self.budget_kib:
                    budget_breached = True
                    termination_verified = self.terminate_active_group()
                    break
                next_measurement = now + MONITOR_INTERVAL_SECONDS

            if leader_status is not None and not self._group_has_live_processes():
                break
            time.sleep(POLL_INTERVAL_SECONDS)

        if leader_status is None:
            leader_status = process.poll()
        if leader_status is None and termination_verified:
            leader_status = process.wait()

        signal_status = self.received_signal
        self._clear_active()

        if signal_status is not None:
            if not termination_verified:
                print("warning: guarded command termination could not be verified", file=sys.stderr)
            return 128 + signal_status

        # Catch commands that cross the limit and finish between measurements.
        if not budget_breached and self.target_size_kib() > self.budget_kib:
            budget_breached = True

        if budget_breached:
            if termination_verified:
                cleanup_status = self.clean_target()
                if self.received_signal is not None:
                    return 128 + self.received_signal
                if cleanup_status != 0:
                    print(
                        f"warning: Cargo artifact cleanup failed with status {cleanup_status}",
                        file=sys.stderr,
                    )
            else:
                print("warning: guarded command termination could not be verified", file=sys.stderr)
            print("error: Cargo artifact budget exceeded; command stopped", file=sys.stderr)
            return BREACH_STATUS

        if self.received_signal is not None:
            return 128 + self.received_signal
        return self.shell_status(leader_status if leader_status is not None else 2)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--repo-dir", required=True, type=Path)
    parser.add_argument("--target-dir", required=True, type=Path)
    parser.add_argument("--budget-kib", required=True, type=int)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required")
    return args


def main() -> int:
    args = parse_args()
    supervisor = Supervisor(args.repo_dir, args.target_dir, args.budget_kib)
    supervisor.install_signal_handlers()
    return supervisor.run(args.command)


if __name__ == "__main__":
    raise SystemExit(main())
