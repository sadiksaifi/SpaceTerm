"""Native tools for a disposable Jammy VM, never a host-service installer.

The owner runs these tools explicitly. Outputs, deadlines, and owned process
retirement are bounded. Downloads are data, and GPG runs against private copies
with an independently pinned keyring and primary signer. No custody collection,
credential delivery, or administrator SpaceTerm command belongs here.
"""

import errno
import hashlib
import json
import os
import re
import selectors
import signal
import stat
import subprocess
import tempfile
import time
from pathlib import Path

from spaceterm_tasks import TaskError, server_artifacts
from spaceterm_tasks.processes import darwin_zombie_group as _darwin_zombie_group

_ISO_BLOCK = 2048
MAX_OUTPUT = 64 * 1024
ENVIRONMENT = {"PATH": "/usr/bin:/bin", "LC_ALL": "C"}


def run_tool(
    command, *, timeout=30, maximum=MAX_OUTPUT, cwd=None, environment=None, reject_stderr=False
):
    """Own one group through its final signal, then finitely consume the child.

    No poll or consuming wait releases the leader before group retirement. Once
    wait begins, a consuming-reap interruption cannot reopen signal authority.
    The caller must have no competing child reaper.
    """
    if not 0 < timeout <= 300 or not 0 < maximum <= 1024 * 1024:
        raise TaskError("invalid-tool-bound")
    if signal.getsignal(signal.SIGCHLD) == signal.SIG_IGN:
        raise TaskError("child-ownership-unavailable")
    process = None
    output = {"stdout": bytearray(), "stderr": bytearray()}
    failure = None
    status = None
    deferred = []
    handlers = {}

    def defer(signum, _frame):
        if not deferred:
            deferred.append(
                KeyboardInterrupt() if signum == signal.SIGINT else SystemExit(128 + signum)
            )

    try:
        # Install before Popen. A task signal cannot strand an acquired child
        # between native spawning and publication of its Python owner.
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            handlers[signum] = signal.signal(signum, defer)
        try:
            process = subprocess.Popen(
                command,
                cwd=cwd,
                env=ENVIRONMENT if environment is None else environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            deadline = time.monotonic() + timeout
            with selectors.DefaultSelector() as selector:
                for name, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
                    assert stream is not None
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ, name)
                while selector.get_map():
                    if deferred:
                        raise deferred[0]
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TaskError("tool-timeout")
                    for key, _ in selector.select(min(remaining, 0.05)):
                        data = os.read(key.fd, 8192)
                        if not data:
                            selector.unregister(key.fileobj)
                        else:
                            if reject_stderr and key.data == "stderr":
                                raise TaskError("tool-reported-error")
                            output[key.data].extend(data)
                            if len(output[key.data]) > maximum:
                                raise TaskError("tool-output-limit")
            if hasattr(os, "WNOWAIT"):
                while (
                    os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is None
                ):
                    if deferred:
                        raise deferred[0]
                    if time.monotonic() >= deadline:
                        raise TaskError("tool-timeout")
                    time.sleep(0.01)
        except BaseException as error:
            failure = error
        finally:
            if process is not None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except OSError as error:
                    if not (error.errno == errno.EPERM and _darwin_zombie_group(process.pid)):
                        if failure is None:
                            failure = TaskError("tool-cleanup-unresolved")
                # Signal authority closes before the first consuming wait. Only
                # reaping can retry, within this original finite deadline.
                reap_deadline = time.monotonic() + 2
                while time.monotonic() < reap_deadline:
                    try:
                        status = process.wait(timeout=max(0.001, reap_deadline - time.monotonic()))
                        break
                    except (KeyboardInterrupt, SystemExit) as error:
                        if failure is None:
                            failure = error
                    except (OSError, subprocess.TimeoutExpired):
                        if failure is None:
                            failure = TaskError("tool-cleanup-unresolved")
                        break
                else:
                    if failure is None:
                        failure = TaskError("tool-cleanup-unresolved")
                for stream in (process.stdout, process.stderr):
                    if stream is not None:
                        stream.close()
    finally:
        for signum, handler in handlers.items():
            signal.signal(signum, handler)
    if failure is None and deferred:
        failure = deferred[0]
    if failure is not None:
        raise failure
    if status:
        raise TaskError("tool-failed")
    return bytes(output["stdout"])


def private_file(path, maximum):
    """Read a bounded, owned, singly linked file without following its final link."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or before.st_nlink != 1
            or stat.S_IMODE(before.st_mode) != 0o600
            or not 0 < before.st_size <= maximum
        ):
            raise TaskError("unsafe-vm-input")
        chunks = bytearray()
        while len(chunks) <= maximum:
            chunk = os.read(fd, min(65536, maximum + 1 - len(chunks)))
            if not chunk:
                break
            chunks.extend(chunk)
        if len(chunks) != before.st_size or os.fstat(fd) != before:
            raise TaskError("vm-input-changed")
        return bytes(chunks)
    finally:
        os.close(fd)


def validate_signer(output, expected):
    if not isinstance(expected, str) or not re.fullmatch(
        r"[A-Fa-f0-9]{40}|[A-Fa-f0-9]{64}", expected
    ):
        raise TaskError("invalid-image-signer")
    valid = []
    try:
        for line in output.decode("ascii").splitlines():
            fields = line.split()
            if not fields or fields[0] != "[GNUPG:]":
                raise TaskError("image-signature-rejected")
            if len(fields) > 1 and fields[1] in (
                "BADSIG",
                "ERRSIG",
                "EXPSIG",
                "EXPKEYSIG",
                "REVKEYSIG",
                "NO_PUBKEY",
                "NODATA",
            ):
                raise TaskError("image-signature-rejected")
            if len(fields) > 1 and fields[1] == "VALIDSIG":
                if len(fields) != 12 or fields[9] not in ("8", "9", "10"):
                    raise TaskError("image-signature-rejected")
                valid.append(fields[-1].upper())
    except UnicodeError:
        raise TaskError("image-signature-rejected") from None
    if valid != [expected.upper()]:
        raise TaskError("image-signature-rejected")


def verify_checksums(checksums, signature, keyring, signer, keyring_sha256):
    """Authenticate exact private checksum copies before parsing their content."""
    checksum_bytes = private_file(checksums, 64 * 1024)
    signature_bytes = private_file(signature, 64 * 1024)
    keys = private_file(keyring, 4 * 1024 * 1024)
    if hashlib.sha256(keys).hexdigest() != keyring_sha256:
        raise TaskError("image-keyring-rejected")
    with tempfile.TemporaryDirectory(prefix="spaceterm-image-trust-") as temporary:
        root = Path(temporary)
        for name, data in (
            ("checksums", checksum_bytes),
            ("signature", signature_bytes),
            ("keyring", keys),
        ):
            path = root / name
            path.write_bytes(data)
            path.chmod(0o600)
        output = run_tool(
            [
                "/usr/bin/gpgv",
                "--homedir",
                root,
                "--status-fd",
                "1",
                "--keyring",
                root / "keyring",
                root / "signature",
                root / "checksums",
            ],
            timeout=15,
            cwd=root,
        )
        validate_signer(output, signer)
    result = {}
    try:
        for line in checksum_bytes.decode("ascii").splitlines():
            match = re.fullmatch(r"([0-9a-f]{64}) [ *]([A-Za-z0-9_][A-Za-z0-9_.+-]{0,254})", line)
            if not match or match[2] in result or len(result) >= 256:
                raise TaskError("image-checksums-rejected")
            result[match[2]] = match[1]
    except UnicodeError:
        raise TaskError("image-checksums-rejected") from None
    if not result:
        raise TaskError("image-checksums-rejected")
    return result


def _both(value: int, width: int) -> bytes:
    return value.to_bytes(width, "little") + value.to_bytes(width, "big")


def _iso_record(name: bytes, extent: int, length: int, *, directory: bool = False) -> bytes:
    size = 33 + len(name) + (not len(name) % 2)
    result = bytearray(size)
    result[0] = size
    result[2:10] = _both(extent, 4)
    result[10:18] = _both(length, 4)
    result[18:25] = bytes((126, 10, 12, 0, 0, 0, 0))
    result[25] = 2 if directory else 0
    result[28:32] = _both(1, 2)
    result[32] = len(name)
    result[33 : 33 + len(name)] = name
    return bytes(result)


def _iso_volume(kind: int, blocks: int, root: int, little_table: int, big_table: int) -> bytes:
    result = bytearray(_ISO_BLOCK)
    result[:7] = bytes((kind,)) + b"CD001\x01"
    wide = kind == 2
    for start, text in ((8, "SPACETERM"), (40, "CIDATA")):
        result[start : start + 32] = (
            text.ljust(16).encode("utf-16-be") if wide else text.ljust(32).encode("ascii")
        )
    result[80:88] = _both(blocks, 4)
    if wide:
        result[88:91] = b"%/E"
    result[120:124] = _both(1, 2)
    result[124:128] = _both(1, 2)
    result[128:132] = _both(_ISO_BLOCK, 2)
    result[132:140] = _both(10, 4)
    result[140:144] = little_table.to_bytes(4, "little")
    result[148:152] = big_table.to_bytes(4, "big")
    result[156:190] = _iso_record(b"\x00", root, _ISO_BLOCK, directory=True)
    result[881] = 1
    return bytes(result)


def _nocloud_iso(user_data: bytes, meta_data: bytes) -> bytes:
    contents = (user_data, meta_data)
    sizes = tuple((len(data) + _ISO_BLOCK - 1) // _ISO_BLOCK for data in contents)
    extents = (25, 25 + sizes[0])
    blocks = extents[1] + sizes[1]
    result = bytearray(blocks * _ISO_BLOCK)
    for index, kind, root, little, big in ((16, 1, 19, 21, 22), (17, 2, 20, 23, 24)):
        result[index * _ISO_BLOCK : (index + 1) * _ISO_BLOCK] = _iso_volume(
            kind, blocks, root, little, big
        )
        records = _iso_record(b"\x00", root, _ISO_BLOCK, directory=True) + _iso_record(
            b"\x01", root, _ISO_BLOCK, directory=True
        )
        for name, extent, data in zip(("user-data", "meta-data"), extents, contents, strict=True):
            identifier = (
                name.encode("utf-16-be") if kind == 2 else (name.upper() + ";1").encode("ascii")
            )
            records += _iso_record(identifier, extent, len(data))
        result[root * _ISO_BLOCK : root * _ISO_BLOCK + len(records)] = records
        for table, endian in ((little, "little"), (big, "big")):
            path_record = (
                b"\x01\x00" + root.to_bytes(4, endian) + (1).to_bytes(2, endian) + b"\x00\x00"
            )
            result[table * _ISO_BLOCK : table * _ISO_BLOCK + 10] = path_record
    result[18 * _ISO_BLOCK : 18 * _ISO_BLOCK + 7] = b"\xffCD001\x01"
    for data, extent in zip(contents, extents, strict=True):
        result[extent * _ISO_BLOCK : extent * _ISO_BLOCK + len(data)] = data
    return bytes(result)


def jammy_seed(token, account_key, host_public_key, host_private_key):
    """One disposable account with normal shells and an independently pinned host key."""
    if not re.fullmatch(r"[0-9a-f]{64}", token) or any(
        not 0 < len(value) <= 8192 for value in (account_key, host_public_key, host_private_key)
    ):
        raise TaskError("invalid-seed-input")
    document = {
        "users": [
            {
                "name": "stvm",
                "uid": 20000,
                "shell": "/bin/bash",
                "lock_passwd": True,
                "ssh_authorized_keys": [account_key.decode("ascii").strip()],
                "sudo": [
                    "ALL=(root) NOPASSWD: /usr/bin/loginctl enable-linger 20000, /usr/sbin/reboot, /usr/sbin/poweroff"
                ],
            }
        ],
        "disable_root": True,
        "ssh_pwauth": False,
        "ssh_deletekeys": True,
        "ssh_genkeytypes": [],
        "ssh_keys": {
            "ed25519_private": host_private_key.decode("ascii"),
            "ed25519_public": host_public_key.decode("ascii").strip(),
        },
    }
    user = b"#cloud-config\n" + json.dumps(document, separators=(",", ":")).encode("ascii")
    meta = json.dumps({"instance-id": "stvm-" + token, "local-hostname": "stvm-jammy"}).encode()
    return _nocloud_iso(user, meta)


GUEST_COMMANDS = {
    "observe": "cat /etc/os-release; getconf GNU_LIBC_VERSION; systemctl --version",
    "status": "spaceterm-server --json status",
    "install": "spaceterm-server --json install",
    "start": "spaceterm-server --json start",
    "stop": "spaceterm-server --json stop",
    "uninstall": "spaceterm-server --json uninstall",
    "reboot": "/usr/bin/sudo -n -- /usr/sbin/reboot",
    "poweroff": "/usr/bin/sudo -n -- /usr/sbin/poweroff",
}


def guest_command(directory, port, operation):
    if operation not in GUEST_COMMANDS or type(port) is not int or not 1024 <= port <= 65535:
        raise TaskError("invalid-guest-operation")
    # Validate before SSH can consume a private key. No agent, TOFU, inherited
    # config, multiplexing, local command, forwarding, or root SpaceTerm process.
    private_file(directory / "id_ed25519", 8192)
    private_file(directory / "known_hosts", 8192)
    return run_tool(
        [
            "/usr/bin/ssh",
            "-F",
            "/dev/null",
            "-p",
            str(port),
            "-i",
            directory / "id_ed25519",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "IdentityAgent=none",
            "-o",
            "BatchMode=yes",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "HostKeyAlgorithms=ssh-ed25519",
            "-o",
            "HostKeyAlias=spaceterm-vm",
            "-o",
            f"UserKnownHostsFile={directory / 'known_hosts'}",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "UpdateHostKeys=no",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "LogLevel=ERROR",
            "-T",
            "stvm@127.0.0.1",
            GUEST_COMMANDS[operation],
        ],
        timeout=30,
    )


class NativeVm:
    """A fixed Docker/QEMU runner. SpaceTerm and QEMU never execute as host root."""

    @staticmethod
    def docker(*arguments):
        return run_tool(
            ["/usr/bin/docker", "--host", "unix:///var/run/docker.sock", *arguments],
            timeout=60,
            maximum=1024 * 1024,
        )

    @staticmethod
    def _disk_info(path):
        info = server_artifacts.strict_json(
            run_tool(["/usr/bin/qemu-img", "info", "--output=json", path]), MAX_OUTPUT
        )
        if (
            not isinstance(info, dict)
            or info.get("format") != "qcow2"
            or type(info.get("virtual-size")) is not int
            or info["virtual-size"] <= 0
            or info.get("backing-filename")
            or info.get("data-file")
        ):
            raise TaskError("unsafe-vm-disk")
        return info["virtual-size"]

    def prepare(self, directory, image, resources, token):
        maximum = resources.disk_gib * 1024**3
        if self._disk_info(image) > maximum:
            raise TaskError("vm-source-disk-exceeds-bound")
        disk = directory / "os.qcow2"
        run_tool(
            ["/usr/bin/qemu-img", "convert", "-f", "qcow2", "-O", "qcow2", image, disk], timeout=300
        )
        disk.chmod(0o600)
        run_tool(["/usr/bin/qemu-img", "resize", disk, f"{resources.disk_gib}G"])
        if self._disk_info(disk) != maximum:
            raise TaskError("unsafe-vm-disk")
        for name in ("id_ed25519", "host_ed25519"):
            run_tool(
                [
                    "/usr/bin/ssh-keygen",
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-C",
                    "",
                    "-f",
                    directory / name,
                ]
            )
            (directory / name).chmod(0o600)
            (directory / (name + ".pub")).chmod(0o600)
        account = private_file(directory / "id_ed25519.pub", 8192)
        host_public = private_file(directory / "host_ed25519.pub", 8192)
        host_private = private_file(directory / "host_ed25519", 8192)
        seed = directory / "seed.iso"
        seed.write_bytes(jammy_seed(token, account, host_public, host_private))
        seed.chmod(0o600)
        known = directory / "known_hosts"
        known.write_bytes(b"spaceterm-vm " + host_public.strip() + b"\n")
        known.chmod(0o600)
        for name in ("id_ed25519.pub", "host_ed25519", "host_ed25519.pub"):
            (directory / name).unlink()

    @staticmethod
    def guest_arguments(resources):
        return [
            "-enable-kvm",
            "-nographic",
            "-smp",
            str(resources.cpus),
            "-m",
            str(resources.memory_mib),
            "-drive",
            "file=/vm/os.qcow2,if=virtio,format=qcow2",
            "-drive",
            "file=/vm/seed.iso,format=raw,media=cdrom,readonly=on",
            "-netdev",
            "user,id=net0,hostfwd=tcp:0.0.0.0:2222-:22",
            "-device",
            "virtio-net-pci,netdev=net0",
        ]

    def create(self, directory, runner_image, resources, port, token):
        kvm_gid = os.stat("/dev/kvm").st_gid
        output = self.docker(
            "create",
            "--pull=never",
            "--user",
            f"{os.getuid()}:{os.getgid()}",
            "--group-add",
            str(kvm_gid),
            "--device",
            "/dev/kvm:/dev/kvm:rw",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--network=bridge",
            "--ipc=private",
            "--cpus=2",
            "--memory=6g",
            "--memory-swap=6g",
            "--pids-limit=256",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,size=64m",
            "--restart=no",
            "--no-healthcheck",
            "--log-driver=json-file",
            "--log-opt",
            "max-size=1m",
            "--log-opt",
            "max-file=1",
            "--mount",
            f"type=bind,source={directory},target=/vm",
            "--publish",
            f"127.0.0.1:{port}:2222",
            "--label",
            f"spaceterm.vm.owner={token}",
            "--entrypoint",
            "/usr/bin/qemu-system-x86_64",
            runner_image,
            *self.guest_arguments(resources),
        )
        identifier = output.strip().decode("ascii")
        if not re.fullmatch(r"[0-9a-f]{64}", identifier):
            raise TaskError("vm-creation-unresolved")
        return identifier

    def inspect(self, identifier):
        output = server_artifacts.strict_json(
            self.docker("container", "inspect", identifier), 1024 * 1024
        )
        if not isinstance(output, list) or len(output) != 1 or not isinstance(output[0], dict):
            raise TaskError("vm-observation-rejected")
        return output[0]

    def validate(self, observation, directory, runner_image, resources, port, token):
        config, host = observation.get("Config", {}), observation.get("HostConfig", {})
        mounts = observation.get("Mounts", [])
        if (
            config.get("Image") != runner_image
            or config.get("User") != f"{os.getuid()}:{os.getgid()}"
            or config.get("Entrypoint") != ["/usr/bin/qemu-system-x86_64"]
            or config.get("Cmd") != self.guest_arguments(resources)
            or config.get("Labels", {}).get("spaceterm.vm.owner") != token
            or host.get("Privileged") is not False
            or host.get("ReadonlyRootfs") is not True
            or host.get("CapDrop") != ["ALL"]
            or host.get("CapAdd") not in (None, [])
            or host.get("NetworkMode") != "bridge"
            or host.get("PidMode") not in ("", "private")
            or host.get("IpcMode") != "private"
            or host.get("NanoCpus") != 2 * 10**9
            or host.get("Memory") != 6 * 1024**3
            or host.get("MemorySwap") != 6 * 1024**3
            or host.get("PidsLimit") != 256
            or host.get("PortBindings")
            != {"2222/tcp": [{"HostIp": "127.0.0.1", "HostPort": str(port)}]}
            or host.get("RestartPolicy", {}).get("Name") != "no"
            or host.get("VolumesFrom") not in (None, [])
            or len(mounts) != 1
            or mounts[0].get("Type") != "bind"
            or mounts[0].get("Source") != str(directory)
            or mounts[0].get("Destination") != "/vm"
            or mounts[0].get("RW") is not True
        ):
            raise TaskError("vm-ownership-mismatch")

    def start(self, identifier):
        self.docker("start", identifier)

    def stop(self, identifier):
        self.docker("stop", "--time", "30", identifier)

    def remove(self, identifier):
        self.docker("rm", identifier)

    def exists(self, identifier):
        output = self.docker(
            "ps", "--all", "--no-trunc", "--filter", "id=" + identifier, "--format", "{{.ID}}"
        )
        if output.strip() not in (b"", identifier.encode()):
            raise TaskError("vm-observation-rejected")
        return bool(output.strip())
