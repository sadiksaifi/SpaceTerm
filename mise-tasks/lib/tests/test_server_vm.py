"""Owned VM admission and lifecycle planning tests, never native VM evidence.

Filesystem paths, identities, manifests, and image hashing stay real. Only detached
signature verification and exact-ID Docker inspection are controlled boundaries.
Synthetic image/key bytes and native observations cannot boot or authenticate a VM.
"""

import hashlib
import io
import json
import os
import tempfile
import unittest
from collections.abc import Callable
from contextlib import redirect_stderr, redirect_stdout
from dataclasses import replace
from pathlib import Path

from spaceterm_tasks.server_vm import (
    MANIFEST_NAME,
    MANIFEST_SCHEMA,
    OWNER_LABEL,
    Classification,
    ContainerConfiguration,
    ContainerObservation,
    Decision,
    DiskEvidence,
    GuestImage,
    OwnedPath,
    Ownership,
    Phase,
    PreparedVm,
    PrepareRequest,
    Resources,
    RunnerIdentity,
    RunnerPolicyObservation,
    SeedEvidence,
    SshEndpoint,
    VmArgumentParser,
    VmArtifacts,
    VmPlan,
    VmPlanner,
    run_task,
)

# These are fixture identities, not authenticated release pins or credentials.
SIGNER = "1" * 40
TOKEN = "a" * 64
CONTAINER_ID = "c" * 64
RUNNER_IMAGE = "registry.example.invalid/qemu@sha256:" + "b" * 64
LOCAL_RUNNER_IMAGE = "sha256:" + "d" * 64


def prepared_artifacts(directory: Path, resources: Resources) -> VmArtifacts:
    """Real private files with controlled offline facts, not qcow2/ISO evidence."""
    disks = []
    for name, size in (
        ("os.qcow2", resources.disk_gib * 1024**3),
        ("encrypted.qcow2", resources.encrypted_disk_gib * 1024**3),
    ):
        path = directory / name
        path.touch(mode=0o600)
        path.write_bytes(b"synthetic standalone disk fixture")
        disks.append(
            DiskEvidence(identity(path), "qcow2", size, False, False, path.stat().st_blocks * 512)
        )
    seed = directory / "seed.iso"
    data = b"synthetic NoCloud CIDATA fixture, not an ISO or credentials"
    seed.touch(mode=0o600)
    seed.write_bytes(data)
    return VmArtifacts(
        disks[0],
        disks[1],
        SeedEvidence(identity(seed), len(data), hashlib.sha256(data).hexdigest(), "CIDATA"),
    )


def bounded_policy() -> RunnerPolicyObservation:
    """Independent expected inspection facts, never production policy helpers."""
    return RunnerPolicyObservation(
        image_id=LOCAL_RUNNER_IMAGE,
        environment=(),
        healthcheck=(),
        declared_volumes=(),
        declared_ports=(),
        working_directory="/vm",
        restart_policy="no",
        restart_retries=0,
        auto_remove=False,
        runtime="runc",
        device_requests=(),
        device_cgroup_rules=(),
        volumes_from=(),
        bind_propagation="rprivate",
        recursive_submounts=False,
        cgroup_namespace="private",
        seccomp_profile="default",
        apparmor_profile="docker-default",
        resource_overrides=(),
        oom_kill_disable=False,
        oom_score_adj=0,
        sysctls=(),
        cgroup_parent="",
        storage_options=(),
        stop_signal="SIGTERM",
        stop_timeout_seconds=30,
    )


def bounded_configuration(
    directory: Path, runner: RunnerIdentity, runner_image: str = RUNNER_IMAGE
) -> ContainerConfiguration:
    return ContainerConfiguration(
        runner_image=runner_image,
        runner=runner,
        cpus=2,
        memory_mib=6144,
        memory_swap_mib=6144,
        pids_limit=256,
        read_only=True,
        privileged=False,
        cap_drop=("ALL",),
        cap_add=(),
        security_options=("no-new-privileges:true",),
        supplemental_groups=(runner.kvm_gid,),
        tmpfs=("/tmp:rw,noexec,nosuid,size=64m",),
        devices=("/dev/kvm:/dev/kvm:rw",),
        bind_mounts=((directory, "/vm"),),
        volume_mounts=(),
        namespaces=("bridge", "", "private", "", ""),
        publications=("127.0.0.1:25022:2222/tcp",),
        publish_all=False,
        log_driver="local",
        log_options=("max-size=10m", "max-file=2"),
        entrypoint=("/usr/bin/qemu-system-x86_64",),
        arguments=(
            "-nodefaults",
            "-no-user-config",
            "-display",
            "none",
            "-monitor",
            "none",
            "-accel",
            "kvm",
            "-machine",
            "q35",
            "-cpu",
            "host",
            "-smp",
            "2",
            "-m",
            "4096",
            "-drive",
            "file=/vm/os.qcow2,format=qcow2,if=virtio",
            "-drive",
            "file=/vm/encrypted.qcow2,format=qcow2,if=virtio",
            "-drive",
            "file=/vm/seed.iso,format=raw,if=ide,media=cdrom,readonly=on",
            "-netdev",
            "user,id=net0,hostfwd=tcp:0.0.0.0:2222-:22",
            "-device",
            "virtio-net-pci,netdev=net0",
            "-qmp",
            "unix:/vm/qmp.sock,server=on,wait=off",
            "-serial",
            "stdio",
        ),
    )


class ControlledSignatures:
    def __init__(self):
        self.fingerprint: str | None = SIGNER
        self.reads: list[tuple[bytes, bytes]] = []
        self.error: Exception | None = None
        self.on_read: Callable[[], None] | None = None

    def verify(self, checksums: bytes, signature: bytes) -> str | None:
        self.reads.append((checksums, signature))
        if self.error is not None:
            raise self.error
        if self.on_read is not None:
            self.on_read()
        return self.fingerprint


class ContentCheckingSignatures:
    """Only one synthetic checksum/signature byte pair authenticates the fixture signer."""

    def __init__(self, signed_checksums: bytes, signed_signature: bytes):
        self.signed_pair = (signed_checksums, signed_signature)
        self.arguments: list[tuple[bytes, bytes]] = []
        self.verified_pairs: list[tuple[bytes, bytes]] = []
        self.on_read: Callable[[], None] | None = None
        self.after_read: Callable[[], None] | None = None

    def verify(self, checksums: bytes, signature: bytes) -> str | None:
        self.arguments.append((checksums, signature))
        if self.on_read is not None:
            self.on_read()
        try:
            pair = (checksums, signature)
            self.verified_pairs.append(pair)
            return SIGNER if pair == self.signed_pair else None
        finally:
            if self.after_read is not None:
                self.after_read()


class ControlledContainers:
    def __init__(self):
        self.observation: ContainerObservation | None = None
        self.reads: list[str] = []
        self.error: Exception | None = None
        self.on_read: Callable[[], None] | None = None

    def inspect(self, container_id: str) -> ContainerObservation | None:
        self.reads.append(container_id)
        if self.error is not None:
            raise self.error
        if self.on_read is not None:
            self.on_read()
        return self.observation


def identity(path: Path) -> OwnedPath:
    return OwnedPath.capture(path)


def manifest_path_record(owned: OwnedPath) -> dict:
    return {"path": str(owned.path), "device": owned.device, "inode": owned.inode}


def manifest_artifact_record(artifacts: VmArtifacts | None) -> dict | None:
    """Independent fixture for the retained artifact format, not production policy."""
    if artifacts is None:
        return None
    disks = {}
    for name, disk in (
        ("os_disk", artifacts.os_disk),
        ("encrypted_disk", artifacts.encrypted_disk),
    ):
        disks[name] = {
            "file": manifest_path_record(disk.file),
            "format": disk.format,
            "virtual_size_bytes": disk.virtual_size_bytes,
            "has_backing_file": disk.has_backing_file,
            "has_external_data_file": disk.has_external_data_file,
            "allocated_bytes": disk.allocated_bytes,
        }
    return {
        **disks,
        "seed": {
            "file": manifest_path_record(artifacts.seed.file),
            "size_bytes": artifacts.seed.size_bytes,
            "sha256": artifacts.seed.sha256,
            "label": artifacts.seed.label,
        },
    }


class VmPlanningTests(unittest.TestCase):
    def setUp(self):
        # Fixture files must not inherit a group-writable host umask such as Debian's 0002.
        self.addCleanup(os.umask, os.umask(0o022))
        self.temporary = tempfile.TemporaryDirectory(prefix="spaceterm-vm-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.checkout = self.root / "checkout"
        self.checkout.mkdir(mode=0o700)
        self.directory = self.root / "vm"
        self.directory.mkdir(mode=0o700)
        # Each simulated downloaded object has a separate new empty directory.
        image_dir = self.root / "image-download"
        checksum_dir = self.root / "checksum-download"
        signature_dir = self.root / "signature-download"
        for path in (image_dir, checksum_dir, signature_dir):
            path.mkdir(mode=0o700)
        image = image_dir / "ubuntu-22.04.img"
        data = b"synthetic image fixture, not Ubuntu or native evidence\n"
        image.write_bytes(data)
        self.digest = hashlib.sha256(data).hexdigest()
        checksums = checksum_dir / "SHA256SUMS"
        checksums.write_text(f"{self.digest} *{image.name}\n", encoding="ascii")
        signature = signature_dir / "SHA256SUMS.gpg"
        signature.write_bytes(b"synthetic signature handled only by the controlled verifier")
        self.request = PrepareRequest(
            directory=self.directory,
            image=GuestImage(image, checksums, signature, self.digest, SIGNER, len(data)),
            runner_image=RUNNER_IMAGE,
        )
        self.signatures = ControlledSignatures()
        self.containers = ControlledContainers()
        # Native numeric IDs vary. No group is created or changed by the test.
        self.runner = RunnerIdentity(os.getuid() or 1000, os.getgid() or 1000, 107)
        self.planner = VmPlanner(
            checkout=self.checkout,
            signatures=self.signatures,
            containers=self.containers,
            runner=self.runner,
            runner_image_pin=RUNNER_IMAGE,
            runner_image_id_pin=LOCAL_RUNNER_IMAGE,
        )

    def assert_planned(self, decision: Decision, phase: Phase) -> VmPlan:
        self.assertEqual(decision.phase, phase)
        self.assertEqual(decision.classification, Classification.PLANNED)
        if decision.plan is None:
            self.fail("admitted planning must return a proposal")
        self.assertEqual(decision.plan.phase, phase)
        return decision.plan

    def assert_denied(self, decision: Decision, classification: Classification):
        self.assertEqual(decision.classification, classification)
        self.assertIsNone(decision.plan)

    def assert_files_unchanged(self, retained: tuple[OwnedPath, ...]):
        for original in retained:
            current = identity(original.path)
            self.assertEqual(current, original)
            self.assertEqual(current.ancestors, original.ancestors)
            self.assertEqual(current.content_sha256, original.content_sha256)

    def assert_stamps_unchanged(self, retained: tuple[tuple[Path, os.stat_result], ...]):
        # Reading files can change access times, which admission does not retain.
        attributes = (
            "st_dev",
            "st_ino",
            "st_mode",
            "st_uid",
            "st_gid",
            "st_nlink",
            "st_size",
            "st_mtime_ns",
            "st_ctime_ns",
        )
        for path, original in retained:
            current = path.stat()
            self.assertEqual(
                tuple(getattr(current, attribute) for attribute in attributes),
                tuple(getattr(original, attribute) for attribute in attributes),
            )

    def write_manifest(self, vm: PreparedVm):
        """Fixture for the intentional version-1 receipt format, not a policy mock."""
        owner = vm.ownership
        request = vm.request
        document = {
            "schema": MANIFEST_SCHEMA,
            "token": owner.token,
            "checkout": manifest_path_record(owner.checkout),
            "directory": manifest_path_record(owner.directory),
            "manifest": manifest_path_record(owner.manifest),
            "credential_directory": manifest_path_record(owner.credential_directory),
            "credential_files": [manifest_path_record(path) for path in owner.credential_files],
            "container_id": owner.container_id,
            "runner_image": request.runner_image,
            "guest_sha256": request.image.sha256,
            "guest_size_bytes": request.image.size_bytes,
            "guest_signer_fingerprint": request.image.signer_fingerprint,
            "artifacts": manifest_artifact_record(vm.artifacts),
            "resources": {
                "cpus": request.resources.cpus,
                "memory_mib": request.resources.memory_mib,
                "disk_gib": request.resources.disk_gib,
                "encrypted_disk_gib": request.resources.encrypted_disk_gib,
                "boot_timeout_seconds": request.resources.boot_timeout_seconds,
            },
            "ssh": {"address": request.ssh.address, "port": request.ssh.port},
        }
        owner.manifest.path.write_text(json.dumps(document), encoding="utf-8")

    def prepared(self, *, container_id: str | None = CONTAINER_ID) -> PreparedVm:
        credentials = self.directory / "credentials"
        credentials.mkdir(mode=0o700)
        private_key = credentials / "id_ed25519"
        public_key = credentials / "id_ed25519.pub"
        for path in (private_key, public_key):
            path.touch(mode=0o600)
            path.write_bytes(b"synthetic disposable key fixture")
        manifest = self.directory / MANIFEST_NAME
        manifest.touch(mode=0o600)
        owner = Ownership(
            token=TOKEN,
            checkout=identity(self.checkout),
            directory=identity(self.directory),
            manifest=identity(manifest),
            credential_directory=identity(credentials),
            credential_files=(identity(private_key), identity(public_key)),
            container_id=container_id,
        )
        vm = PreparedVm(
            self.request, owner, prepared_artifacts(self.directory, self.request.resources)
        )
        self.write_manifest(vm)
        if container_id is not None:
            self.containers.observation = ContainerObservation(
                container_id, TOKEN, self.directory, running=False, policy=bounded_policy()
            )
        return vm

    def test_bounded_preparation_plans_without_executing_or_writing_credentials(self):
        plan = self.assert_planned(self.planner.prepare(self.request), Phase.PREPARE)
        self.assertEqual(list(self.directory.iterdir()), [])
        self.assertTrue(self.request.image.path.is_file())
        self.assertIsNone(plan.remove_directory)
        self.assertEqual(
            self.signatures.reads,
            [
                (
                    self.request.image.checksums.read_bytes(),
                    self.request.image.signature.read_bytes(),
                )
            ],
        )
        self.assertEqual(self.containers.reads, [])

    def test_prepare_rejects_unbounded_resources_before_native_image_inspection(self):
        cases = (
            ("cpus", 0),
            ("cpus", 3),
            ("cpus", True),
            ("memory_mib", 0),
            ("memory_mib", 1023),
            ("memory_mib", 4097),
            ("disk_gib", 0),
            ("disk_gib", 7),
            ("disk_gib", 25),
            ("encrypted_disk_gib", 0),
            ("encrypted_disk_gib", 9),
            ("boot_timeout_seconds", 0),
            ("boot_timeout_seconds", 14),
            ("boot_timeout_seconds", 301),
        )
        for name, value in cases:
            with self.subTest(resource=name, value=value):
                resources = replace(Resources(), **{name: value})
                request = replace(self.request, resources=resources)
                self.assert_denied(self.planner.prepare(request), Classification.INVALID_RESOURCES)
                self.assertEqual(self.signatures.reads, [])
                self.assertEqual(list(self.directory.iterdir()), [])

    def test_prepare_requires_authenticated_matching_guest_digest(self):
        for pin, signer in (
            (None, SIGNER),
            ("", SIGNER),
            ("0" * 64, SIGNER),
            (self.digest, None),
            (self.digest, "2" * 40),
        ):
            with self.subTest(pin=pin, signer=signer):
                self.signatures.fingerprint = signer
                request = replace(self.request, image=replace(self.request.image, sha256=pin))
                self.assert_denied(self.planner.prepare(request), Classification.UNVERIFIED_IMAGE)
                self.assertEqual(list(self.directory.iterdir()), [])

    def test_signed_checksum_must_name_the_same_digest_and_image(self):
        self.request.image.checksums.write_text(
            f"{'0' * 64} *{self.request.image.path.name}\n", encoding="ascii"
        )
        self.assert_denied(self.planner.prepare(self.request), Classification.UNVERIFIED_IMAGE)

    def test_prepare_rejects_mutable_or_missing_container_digest(self):
        for runner_image in ("ubuntu:22.04", "qemu:latest", "qemu@sha256:bad"):
            with self.subTest(runner_image=runner_image):
                request = replace(self.request, runner_image=runner_image)
                self.assert_denied(self.planner.prepare(request), Classification.UNVERIFIED_IMAGE)

    def test_prepare_rejects_checkout_nonempty_and_argument_unsafe_directories(self):
        checkout_vm = self.checkout / "vm"
        checkout_vm.mkdir(mode=0o700)
        nonempty = self.root / "existing-vm"
        nonempty.mkdir(mode=0o700)
        sentinel = nonempty / "foreign-data"
        sentinel.write_text("retain me", encoding="ascii")
        comma_path = self.root / "vm,readonly"
        comma_path.mkdir(mode=0o700)
        for directory in (checkout_vm, nonempty, comma_path, Path("relative-vm")):
            with self.subTest(directory=directory.name):
                request = replace(self.request, directory=directory)
                self.assert_denied(self.planner.prepare(request), Classification.UNSAFE_PATH)
                self.assertEqual(sentinel.read_text(encoding="ascii"), "retain me")

    def test_prepare_does_not_follow_a_directory_symlink(self):
        linked = self.root / "vm-link"
        linked.symlink_to(self.directory, target_is_directory=True)
        request = replace(self.request, directory=linked)
        self.assert_denied(self.planner.prepare(request), Classification.UNSAFE_PATH)
        self.assertEqual(list(self.directory.iterdir()), [])

    def test_prepare_rejects_nonloopback_and_privileged_ssh_publication(self):
        for endpoint in (
            SshEndpoint("0.0.0.0", 25022),
            SshEndpoint("::", 25022),
            SshEndpoint("::1", 25022),
            SshEndpoint("192.168.1.10", 25022),
            SshEndpoint("localhost", 25022),
            SshEndpoint("127.0.0.1", 0),
            SshEndpoint("127.0.0.1", 22),
            SshEndpoint("127.0.0.1", 65536),
        ):
            with self.subTest(endpoint=endpoint):
                request = replace(self.request, ssh=endpoint)
                self.assert_denied(self.planner.prepare(request), Classification.INVALID_ENDPOINT)

    def test_start_plan_is_bounded_nonroot_and_exposes_only_owned_vm_and_kvm(self):
        vm = self.prepared(container_id=None)
        plan = self.assert_planned(self.planner.start(vm), Phase.START)
        self.assertEqual(plan.ownership, vm.ownership)
        self.assertEqual(plan.ssh, SshEndpoint("127.0.0.1", 25022))
        self.assertEqual(len(plan.commands), 1)
        command = plan.commands[0]
        self.assertEqual(command[:2], ("docker", "run"))

        def values(option: str) -> list[str]:
            return [command[index + 1] for index, item in enumerate(command[:-1]) if item == option]

        self.assertEqual(values("--cpus"), ["2"])
        self.assertEqual(values("--memory"), ["6g"])
        self.assertEqual(values("--memory-swap"), ["6g"])
        self.assertEqual(values("--pids-limit"), ["256"])
        self.assertEqual(values("--user"), [f"{self.runner.uid}:{self.runner.gid}"])
        self.assertEqual(values("--group-add"), [str(self.runner.kvm_gid)])
        self.assertIn("--read-only", command)
        self.assertEqual(values("--cap-drop"), ["ALL"])
        self.assertIn("no-new-privileges:true", values("--security-opt"))
        self.assertEqual(values("--tmpfs"), ["/tmp:rw,noexec,nosuid,size=64m"])
        self.assertEqual(values("--device"), ["/dev/kvm:/dev/kvm:rw"])
        self.assertEqual(values("--mount"), [f"type=bind,src={self.directory},dst=/vm"])
        self.assertEqual(values("--publish"), ["127.0.0.1:25022:2222/tcp"])
        self.assertIn(f"{OWNER_LABEL}={TOKEN}", values("--label"))
        self.assertEqual(values("--log-driver"), ["local"])
        self.assertEqual(set(values("--log-opt")), {"max-size=10m", "max-file=2"})
        self.assertIn(RUNNER_IMAGE, command)
        self.assertEqual(values("-smp"), ["2"])
        self.assertEqual(values("-m"), ["4096"])
        qmp = values("-qmp")
        self.assertEqual(len(qmp), 1)
        self.assertTrue(qmp[0].startswith("unix:/vm/"))
        self.assertNotIn("tcp:", qmp[0])
        for forbidden in (
            "--privileged",
            "--pid",
            "--ipc",
            "--uts",
            "--userns",
            "--volumes-from",
            "--volume",
            "-v",
            "--publish-all",
            "-P",
        ):
            self.assertNotIn(forbidden, command)
        self.assertNotIn("host", values("--network"))
        for argument in command:
            self.assertFalse(argument.startswith(("--network=host", "--pid=host", "--ipc=host")))
            self.assertNotIn("seccomp=unconfined", argument)
            self.assertNotIn("apparmor=unconfined", argument)
            self.assertNotIn("docker.sock", argument)
            self.assertNotIn(str(self.checkout), argument)
        self.assertIsNone(plan.remove_directory)

    def test_stop_and_remove_plan_only_the_exact_container_and_owned_directory(self):
        vm = self.prepared()
        self.containers.observation = ContainerObservation(
            CONTAINER_ID, TOKEN, self.directory, True
        )
        stop = self.assert_planned(self.planner.stop(vm), Phase.STOP)
        self.assertEqual(stop.ownership, vm.ownership)
        self.assertEqual(stop.commands, (("docker", "stop", "--time", "30", CONTAINER_ID),))
        self.assertIsNone(stop.remove_directory)
        self.containers.observation = replace(self.containers.observation, running=False)
        removal = self.assert_planned(self.planner.remove(vm), Phase.REMOVE)
        self.assertEqual(removal.ownership, vm.ownership)
        self.assertEqual(removal.commands, (("docker", "rm", CONTAINER_ID),))
        self.assertEqual(removal.remove_directory, self.directory)
        self.assertEqual(self.containers.reads, [CONTAINER_ID, CONTAINER_ID])
        self.assertTrue(vm.ownership.manifest.path.is_file())
        for credential in vm.ownership.credential_files:
            self.assertTrue(credential.path.is_file())

    def test_remove_requires_an_explicit_stop_not_force_removal(self):
        vm = self.prepared()
        self.containers.observation = ContainerObservation(
            CONTAINER_ID, TOKEN, self.directory, True
        )
        self.assert_denied(self.planner.remove(vm), Classification.INVALID_STATE)
        self.assertTrue(self.directory.is_dir())

    def test_cleanup_refuses_a_foreign_checkout_even_with_a_matching_manifest(self):
        vm = self.prepared()
        foreign_checkout = self.root / "other-checkout"
        foreign_checkout.mkdir(mode=0o700)
        vm = replace(vm, ownership=replace(vm.ownership, checkout=identity(foreign_checkout)))
        self.write_manifest(vm)
        for operation in (self.planner.stop, self.planner.remove):
            with self.subTest(operation=operation.__name__):
                self.assert_denied(operation(vm), Classification.OWNERSHIP_MISMATCH)
                self.assertTrue(self.directory.is_dir())

    def test_cleanup_refuses_replaced_vm_directory_at_the_same_path(self):
        vm = self.prepared()
        retained = self.root / "original-vm"
        self.directory.rename(retained)
        self.directory.mkdir(mode=0o700)
        foreign = self.directory / "foreign-data"
        foreign.write_text("retain replaced directory", encoding="ascii")
        for operation in (self.planner.stop, self.planner.remove):
            with self.subTest(operation=operation.__name__):
                self.assert_denied(operation(vm), Classification.OWNERSHIP_MISMATCH)
                self.assertTrue((retained / MANIFEST_NAME).is_file())
                self.assertTrue(foreign.is_file())
        self.assertEqual(self.containers.reads, [])

    def test_cleanup_refuses_a_symlink_replacing_the_vm_directory(self):
        vm = self.prepared()
        retained = self.root / "original-vm"
        self.directory.rename(retained)
        self.directory.symlink_to(retained, target_is_directory=True)
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertTrue((retained / MANIFEST_NAME).is_file())

    def test_cleanup_refuses_a_replaced_manifest_even_if_its_contents_match(self):
        vm = self.prepared()
        manifest = vm.ownership.manifest.path
        saved = manifest.with_name("retained-manifest")
        manifest.rename(saved)
        manifest.write_bytes(saved.read_bytes())
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertTrue(manifest.is_file())
        self.assertTrue(saved.is_file())

    def test_cleanup_refuses_a_manifest_with_a_foreign_owner_token(self):
        vm = self.prepared()
        manifest = vm.ownership.manifest.path
        document = json.loads(manifest.read_text(encoding="utf-8"))
        document["token"] = "d" * 64
        manifest.write_text(json.dumps(document), encoding="utf-8")
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertTrue(manifest.is_file())

    def test_cleanup_refuses_a_replaced_credential_file(self):
        vm = self.prepared()
        credential = vm.ownership.credential_files[0].path
        retained = credential.with_name("retained-key")
        credential.rename(retained)
        credential.write_bytes(b"different synthetic credential")
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertTrue(credential.is_file())
        self.assertTrue(retained.is_file())

    def test_cleanup_refuses_credentials_outside_the_vm_even_with_matching_receipt(self):
        vm = self.prepared()
        foreign = self.root / "foreign-credentials"
        foreign.mkdir(mode=0o700)
        credential = foreign / "id_ed25519"
        credential.write_bytes(b"foreign synthetic key")
        owner = replace(
            vm.ownership,
            credential_directory=identity(foreign),
            credential_files=(identity(credential),),
        )
        vm = replace(vm, ownership=owner)
        self.write_manifest(vm)
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertTrue(credential.is_file())

    def test_cleanup_refuses_foreign_container_id_label_or_mount(self):
        vm = self.prepared()
        original = ContainerObservation(CONTAINER_ID, TOKEN, self.directory, running=False)
        for observation in (
            replace(original, container_id="d" * 64),
            replace(original, owner_token=None),
            replace(original, owner_token="d" * 64),
            replace(original, mounted_directory=self.checkout),
        ):
            with self.subTest(observation=observation):
                self.containers.observation = observation
                for operation in (self.planner.stop, self.planner.remove):
                    self.assert_denied(operation(vm), Classification.OWNERSHIP_MISMATCH)
                self.assertTrue(vm.ownership.manifest.path.is_file())

    def test_runner_digest_requires_an_independent_pin(self):
        other_pin = "registry.example.invalid/qemu@sha256:" + "e" * 64
        for pin in (None, "qemu:latest", other_pin):
            with self.subTest(pin_kind="missing" if pin is None else "nonmatching"):
                planner = VmPlanner(
                    self.checkout,
                    self.signatures,
                    self.containers,
                    self.runner,
                    runner_image_pin=pin,
                )
                self.assert_denied(planner.prepare(self.request), Classification.UNVERIFIED_IMAGE)
                self.assertEqual(self.signatures.reads, [])

    def test_runner_identity_requires_bounded_nonroot_numeric_ids(self):
        for name in ("uid", "gid", "kvm_gid"):
            for value in (0, -1, True, 2**31):
                with self.subTest(field=name, value=value):
                    runner = replace(self.runner, **{name: value})
                    planner = VmPlanner(
                        self.checkout,
                        self.signatures,
                        self.containers,
                        runner,
                        runner_image_pin=RUNNER_IMAGE,
                    )
                    self.assert_denied(
                        planner.prepare(self.request), Classification.INVALID_RESOURCES
                    )
                    self.assertEqual(self.signatures.reads, [])

    def test_missing_native_adapters_fail_closed(self):
        for signatures, runner in ((None, self.runner), (self.signatures, None)):
            planner = VmPlanner(
                self.checkout,
                signatures,
                self.containers,
                runner,
                runner_image_pin=RUNNER_IMAGE,
            )
            self.assert_denied(planner.prepare(self.request), Classification.NATIVE_UNAVAILABLE)
        vm = self.prepared()
        planner = VmPlanner(
            self.checkout,
            self.signatures,
            None,
            self.runner,
            runner_image_pin=RUNNER_IMAGE,
        )
        for operation in (planner.start, planner.stop, planner.remove):
            self.assert_denied(operation(vm), Classification.NATIVE_UNAVAILABLE)
        self.assertTrue(self.directory.is_dir())

    def test_prepare_rejects_nonprivate_directories_and_other_checkouts(self):
        self.directory.chmod(0o755)
        self.assert_denied(self.planner.prepare(self.request), Classification.UNSAFE_PATH)
        self.directory.chmod(0o700)
        (self.root / ".git").touch()
        self.assert_denied(self.planner.prepare(self.request), Classification.UNSAFE_PATH)
        self.assertEqual(self.signatures.reads, [])

    def test_prepare_rejects_checkout_marker_created_during_signature_verification(self):
        self.assert_planned(self.planner.prepare(self.request), Phase.PREPARE)
        self.signatures.reads.clear()
        image = self.request.image
        paths = (
            self.checkout,
            self.directory,
            image.path.parent,
            image.checksums.parent,
            image.signature.parent,
            image.path,
            image.checksums,
            image.signature,
        )
        retained = tuple(identity(path) for path in paths)
        stamps = tuple((path, path.stat()) for path in paths)
        ancestor = identity(self.root)
        signed_pair = (image.checksums.read_bytes(), image.signature.read_bytes())
        marker = self.root / ".git"
        self.assertFalse(marker.exists())

        def create_checkout_marker() -> None:
            marker.touch(mode=0o600)

        self.signatures.on_read = create_checkout_marker
        decision = self.planner.prepare(self.request)
        # Only the ancestor's checkout status changes, not any retained leaf stamp.
        self.assertTrue(marker.is_file())
        self.assertEqual(identity(self.root), ancestor)
        self.assert_files_unchanged(retained)
        self.assert_stamps_unchanged(stamps)
        self.assertEqual(self.signatures.reads, [signed_pair])
        self.assertEqual(self.containers.reads, [])
        self.assertEqual(list(self.directory.iterdir()), [])
        self.signatures.on_read = None
        self.assert_denied(self.planner.prepare(self.request), Classification.UNSAFE_PATH)
        self.assertEqual(self.signatures.reads, [signed_pair])
        self.assertEqual(decision.phase, Phase.PREPARE)
        self.assert_denied(decision, Classification.UNSAFE_PATH)

    def test_prepare_rejects_argument_injection_in_any_directory_component(self):
        for component in (
            "unsafe,readonly",
            "unsafe:mount",
            "unsafe=mount",
            "-option",
            "unsafe\nline",
        ):
            with self.subTest(case="argument-component"):
                parent = self.root / component
                parent.mkdir(mode=0o700)
                directory = parent / "vm"
                directory.mkdir(mode=0o700)
                request = replace(self.request, directory=directory)
                self.assert_denied(self.planner.prepare(request), Classification.UNSAFE_PATH)
        self.assertEqual(self.signatures.reads, [])

    def test_prepare_rejects_a_symlinked_ancestor(self):
        linked = self.root / "linked-parent"
        linked.symlink_to(self.root, target_is_directory=True)
        request = replace(self.request, directory=linked / "vm")
        self.assert_denied(self.planner.prepare(request), Classification.UNSAFE_PATH)
        self.assertEqual(self.signatures.reads, [])

    def test_image_objects_require_separate_private_directories_and_no_symlinks(self):
        colocated = self.request.image.path.parent / "SHA256SUMS"
        colocated.write_bytes(self.request.image.checksums.read_bytes())
        request = replace(self.request, image=replace(self.request.image, checksums=colocated))
        self.assert_denied(self.planner.prepare(request), Classification.UNSAFE_PATH)
        linked = self.request.image.signature.with_name("linked-signature")
        linked.symlink_to(self.request.image.signature)
        request = replace(self.request, image=replace(self.request.image, signature=linked))
        self.assert_denied(self.planner.prepare(request), Classification.UNSAFE_PATH)
        self.request.image.signature.parent.chmod(0o755)
        self.assert_denied(self.planner.prepare(self.request), Classification.UNSAFE_PATH)
        self.assertEqual(self.signatures.reads, [])

    def test_signed_checksums_reject_ambiguous_malformed_and_oversized_data(self):
        valid = f"{self.digest} *{self.request.image.path.name}\n".encode("ascii")
        for contents in (
            valid + valid,
            f"{self.digest} *different.img\n".encode("ascii"),
            f"{self.digest} *../{self.request.image.path.name}\n".encode("ascii"),
            valid + b"not a checksum\n",
            b"\xff\n",
            b"x" * (1024 * 1024),
        ):
            with self.subTest(case="signed-checksum-data"):
                self.request.image.checksums.write_bytes(contents)
                self.assert_denied(
                    self.planner.prepare(self.request), Classification.UNVERIFIED_IMAGE
                )
                self.assertEqual(list(self.directory.iterdir()), [])

    def test_image_hash_is_streamed_and_checks_every_byte(self):
        data = b"x" * (2 * 1024 * 1024) + b"last chunk"
        digest = hashlib.sha256(data).hexdigest()
        self.request.image.path.write_bytes(data)
        self.request.image.checksums.write_text(
            f"{digest} *{self.request.image.path.name}\n", encoding="ascii"
        )
        request = replace(
            self.request, image=replace(self.request.image, sha256=digest, size_bytes=len(data))
        )
        plan = self.assert_planned(self.planner.prepare(request), Phase.PREPARE)
        self.assertEqual(plan.prepared_directory, identity(self.directory))
        self.assertIsNotNone(plan.image_evidence)
        with request.image.path.open("r+b") as image:
            image.seek(-1, os.SEEK_END)
            image.write(b"!")
        self.assert_denied(self.planner.prepare(request), Classification.UNVERIFIED_IMAGE)

    def test_signature_verification_cannot_replace_the_checked_files(self):
        original = self.request.image.checksums
        retained = original.with_name("retained-checksums")

        def replace_checksums():
            original.rename(retained)
            original.write_bytes(retained.read_bytes())

        self.signatures.on_read = replace_checksums
        self.assert_denied(self.planner.prepare(self.request), Classification.UNVERIFIED_IMAGE)
        self.assertTrue(retained.is_file())
        self.assertEqual(list(self.directory.iterdir()), [])

    def test_signature_verification_uses_original_bytes_during_temporary_parent_substitution(self):
        image = self.request.image
        originals = (image.checksums.read_bytes(), image.signature.read_bytes())
        files = tuple(identity(path) for path in (image.path, image.checksums, image.signature))
        # This different checksum has a valid synthetic signature. Neither original
        # file belongs to that signed pair, although the original image digest is pinned.
        signed_checksums = f"{'0' * 64} *{image.path.name}\n".encode("ascii")
        signed_signature = b"synthetic trusted signature for the alternate checksum fixture"
        verifier = ContentCheckingSignatures(signed_checksums, signed_signature)
        self.assertEqual(verifier.verify(signed_checksums, signed_signature), SIGNER)
        self.assertIsNone(verifier.verify(*originals))
        verifier.arguments.clear()
        verifier.verified_pairs.clear()

        substitutions = []
        for original, contents, name in (
            (image.checksums, signed_checksums, "signed-checksum-download"),
            (image.signature, signed_signature, "signed-signature-download"),
        ):
            signed_parent = self.root / name
            signed_parent.mkdir(mode=0o700)
            (signed_parent / original.name).write_bytes(contents)
            retained_parent = self.root / f"retained-{original.parent.name}"
            substitutions.append((original.parent, retained_parent, signed_parent))

        def substitute_parents():
            for original_parent, retained_parent, signed_parent in substitutions:
                original_parent.rename(retained_parent)
                signed_parent.rename(original_parent)

        def restore_parents():
            for original_parent, retained_parent, signed_parent in reversed(substitutions):
                original_parent.rename(signed_parent)
                retained_parent.rename(original_parent)

        verifier.on_read = substitute_parents
        verifier.after_read = restore_parents
        planner = VmPlanner(
            checkout=self.checkout,
            signatures=verifier,
            containers=self.containers,
            runner=self.runner,
            runner_image_pin=RUNNER_IMAGE,
        )
        decision = planner.prepare(self.request)
        # Swapping only the parents preserves original file stamps, ancestor identities,
        # and content pins. Those observations cannot authenticate the original byte pair.
        self.assert_files_unchanged(files)
        self.assertEqual(image.sha256, hashlib.sha256(image.path.read_bytes()).hexdigest())
        for original_parent, retained_parent, signed_parent in substitutions:
            self.assertEqual(original_parent.stat().st_mode & 0o777, 0o700)
            self.assertFalse(retained_parent.exists())
            self.assertTrue(signed_parent.is_dir())
        self.assertEqual(list(self.directory.iterdir()), [])
        self.assertEqual(self.containers.reads, [])
        self.assert_denied(decision, Classification.UNVERIFIED_IMAGE)
        self.assertEqual(verifier.arguments, [originals])
        self.assertEqual(verifier.verified_pairs, [originals])

    def test_prepare_and_start_reject_download_directory_chmod_during_signature_verification(self):
        image = self.request.image
        files = tuple(identity(path) for path in (image.path, image.checksums, image.signature))
        for phase in (Phase.PREPARE, Phase.START):
            vm = self.prepared(container_id=None) if phase == Phase.START else None
            for name, path in (
                ("image", image.path),
                ("checksums", image.checksums),
                ("signature", image.signature),
            ):
                with self.subTest(phase=phase.value, download=name):
                    parent = path.parent
                    retained_parent = identity(parent)
                    self.signatures.reads.clear()

                    def make_download_public():
                        parent.chmod(0o777)

                    self.signatures.on_read = make_download_public
                    try:
                        decision = (
                            self.planner.prepare(self.request)
                            if vm is None
                            else self.planner.start(vm)
                        )
                        self.assertEqual(parent.stat().st_mode & 0o777, 0o777)
                        self.assertEqual(identity(parent), retained_parent)
                        self.assert_files_unchanged(files)
                        self.assertEqual(len(self.signatures.reads), 1)
                        self.assertEqual(self.containers.reads, [])
                        if vm is None:
                            self.assertEqual(list(self.directory.iterdir()), [])
                        else:
                            self.assertTrue(vm.ownership.manifest.path.is_file())
                            self.assert_files_unchanged(vm.ownership.credential_files)
                        self.assertIn(
                            decision.classification,
                            (Classification.UNSAFE_PATH, Classification.UNVERIFIED_IMAGE),
                        )
                        self.assertIsNone(decision.plan)
                    finally:
                        parent.chmod(0o700)
                        self.signatures.on_read = None

    def test_native_errors_return_only_bounded_classifications(self):
        native_message = "sensitive native message /private/example credential-fixture"
        self.signatures.error = OSError(native_message)
        output = io.StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            decision = self.planner.prepare(self.request)
        self.assert_denied(decision, Classification.NATIVE_UNAVAILABLE)
        self.assertEqual(output.getvalue(), "")
        self.assertNotIn(native_message, repr(decision))
        self.signatures.error = None
        vm = self.prepared()
        self.containers.error = RuntimeError(native_message)
        self.assert_denied(self.planner.remove(vm), Classification.NATIVE_UNAVAILABLE)
        self.assertTrue(self.directory.is_dir())
        for value in (self.request, self.request.image, vm, vm.ownership, vm.ownership.directory):
            self.assertNotIn(str(self.root), repr(value))
            self.assertNotIn(TOKEN, repr(value))

    def test_start_revalidates_manifest_resources_and_image_pins(self):
        vm = self.prepared(container_id=None)
        manifest = vm.ownership.manifest.path
        document = json.loads(manifest.read_text(encoding="utf-8"))
        document["resources"]["memory_mib"] = 1024
        manifest.write_text(json.dumps(document), encoding="utf-8")
        self.assert_denied(self.planner.start(vm), Classification.OWNERSHIP_MISMATCH)
        self.write_manifest(vm)
        vm.request.image.path.write_bytes(b"substituted image fixture")
        self.assert_denied(self.planner.start(vm), Classification.UNVERIFIED_IMAGE)
        self.assertEqual(self.containers.reads, [])

    def test_start_revalidates_ownership_after_signature_inspection(self):
        vm = self.prepared(container_id=None)

        def change_key():
            vm.ownership.credential_files[0].path.write_bytes(
                b"changed credential at the same inode"
            )

        self.signatures.on_read = change_key
        self.assert_denied(self.planner.start(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.containers.reads, [])

    def test_cleanup_rejects_changed_key_contents_and_permissions(self):
        vm = self.prepared()
        credential = vm.ownership.credential_files[0].path
        original = credential.read_bytes()
        credential.write_bytes(b"changed synthetic key at the same inode")
        for operation in (self.planner.stop, self.planner.remove):
            self.assert_denied(operation(vm), Classification.OWNERSHIP_MISMATCH)
        credential.write_bytes(original)
        credential.chmod(0o644)
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.containers.reads, [])

    def test_cleanup_rejects_symlinked_credentials_and_hardlinked_keys(self):
        vm = self.prepared()
        credential = vm.ownership.credential_files[0].path
        linked = credential.with_name("key-hardlink")
        os.link(credential, linked)
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        linked.unlink()
        credential.rename(linked)
        credential.symlink_to(linked)
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.containers.reads, [])

    def test_cleanup_rejects_replaced_checkout_even_with_a_new_matching_receipt(self):
        vm = self.prepared()
        self.checkout.rename(self.root / "retained-checkout")
        self.checkout.mkdir(mode=0o700)
        vm = replace(vm, ownership=replace(vm.ownership, checkout=identity(self.checkout)))
        self.write_manifest(vm)
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.containers.reads, [])

    def test_cleanup_rejects_changed_ancestry_even_when_all_leaf_inodes_match(self):
        parent = self.root / "owned-parent"
        parent.mkdir(mode=0o700)
        self.directory.rmdir()
        self.directory = parent / "vm"
        self.directory.mkdir(mode=0o700)
        self.request = replace(self.request, directory=self.directory)
        vm = self.prepared()
        retained_parent = self.root / "retained-parent"
        parent.rename(retained_parent)
        parent.mkdir(mode=0o700)
        (retained_parent / "vm").rename(self.directory)
        self.assertEqual(self.directory.lstat().st_ino, vm.ownership.directory.inode)
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.containers.reads, [])
        self.assertTrue(vm.ownership.manifest.path.is_file())

    def test_cleanup_rejects_duplicate_untyped_and_oversized_manifests(self):
        vm = self.prepared()
        manifest = vm.ownership.manifest.path
        original = manifest.read_text(encoding="utf-8")
        document = json.loads(original)
        document["schema"] = True
        for contents in (
            original[:-1] + f', "token": "{TOKEN}"}}',
            json.dumps(document),
            "{" + " " * (1024 * 1024) + "}",
        ):
            with self.subTest(case="manifest-data"):
                manifest.write_text(contents, encoding="utf-8")
                self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.containers.reads, [])

    def test_container_inspection_cannot_change_owned_files(self):
        vm = self.prepared()

        def change_key():
            vm.ownership.credential_files[0].path.write_bytes(
                b"substituted credential during inspection"
            )

        self.containers.on_read = change_key
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        self.assertTrue(self.directory.is_dir())
        self.assertEqual(self.containers.reads, [CONTAINER_ID])

    def test_container_absence_and_missing_exact_id_do_not_grant_cleanup(self):
        vm = self.prepared()
        self.containers.observation = None
        self.assert_denied(self.planner.remove(vm), Classification.OWNERSHIP_MISMATCH)
        vm = replace(vm, ownership=replace(vm.ownership, container_id=None))
        self.write_manifest(vm)
        self.containers.reads.clear()
        for operation in (self.planner.stop, self.planner.remove):
            self.assert_denied(operation(vm), Classification.INVALID_STATE)
        self.assertEqual(self.containers.reads, [])

    def test_existing_container_restart_requires_its_actual_bounded_configuration(self):
        vm = self.prepared()
        observation = self.containers.observation
        self.assertIsNotNone(observation)
        self.assert_denied(self.planner.start(vm), Classification.OWNERSHIP_MISMATCH)
        configuration = ContainerConfiguration(
            runner_image=RUNNER_IMAGE,
            runner=self.runner,
            cpus=2,
            memory_mib=6144,
            memory_swap_mib=6144,
            pids_limit=256,
            read_only=True,
            privileged=False,
            cap_drop=("ALL",),
            cap_add=(),
            security_options=("no-new-privileges:true",),
            supplemental_groups=(self.runner.kvm_gid,),
            tmpfs=("/tmp:rw,noexec,nosuid,size=64m",),
            devices=("/dev/kvm:/dev/kvm:rw",),
            bind_mounts=((self.directory, "/vm"),),
            volume_mounts=(),
            namespaces=("bridge", "", "private", "", ""),
            publications=("127.0.0.1:25022:2222/tcp",),
            publish_all=False,
            log_driver="local",
            log_options=("max-size=10m", "max-file=2"),
            entrypoint=("/usr/bin/qemu-system-x86_64",),
            arguments=(
                "-nodefaults",
                "-no-user-config",
                "-display",
                "none",
                "-monitor",
                "none",
                "-accel",
                "kvm",
                "-machine",
                "q35",
                "-cpu",
                "host",
                "-smp",
                "2",
                "-m",
                "4096",
                "-drive",
                "file=/vm/os.qcow2,format=qcow2,if=virtio",
                "-drive",
                "file=/vm/encrypted.qcow2,format=qcow2,if=virtio",
                "-drive",
                "file=/vm/seed.iso,format=raw,if=ide,media=cdrom,readonly=on",
                "-netdev",
                "user,id=net0,hostfwd=tcp:0.0.0.0:2222-:22",
                "-device",
                "virtio-net-pci,netdev=net0",
                "-qmp",
                "unix:/vm/qmp.sock,server=on,wait=off",
                "-serial",
                "stdio",
            ),
        )
        if observation is None:
            self.fail("fixture must retain an exact container observation")
        self.containers.observation = replace(observation, configuration=configuration)
        plan = self.assert_planned(self.planner.start(vm), Phase.START)
        self.assertEqual(plan.commands, (("docker", "start", CONTAINER_ID),))
        for modified in (
            replace(configuration, memory_swap_mib=8192),
            replace(configuration, privileged=True),
            replace(configuration, namespaces=("host", "", "private", "", "")),
            replace(configuration, runner_image="qemu:latest"),
            replace(configuration, supplemental_groups=(self.runner.kvm_gid, 0)),
            replace(configuration, bind_mounts=((self.checkout, "/vm"),)),
            replace(configuration, volume_mounts=("foreign-volume",)),
            replace(configuration, publications=("0.0.0.0:25022:2222/tcp",)),
        ):
            self.containers.observation = replace(observation, configuration=modified)
            self.assert_denied(self.planner.start(vm), Classification.OWNERSHIP_MISMATCH)
        self.containers.observation = replace(
            observation, configuration=configuration, running=True
        )
        self.assert_denied(self.planner.start(vm), Classification.INVALID_STATE)
        self.containers.observation = replace(observation, configuration=configuration)
        image = vm.request.image
        files = tuple(identity(path) for path in (image.path, image.checksums, image.signature))
        for boundary in ("signature", "container"):
            for name, path in (
                ("image", image.path),
                ("checksums", image.checksums),
                ("signature", image.signature),
            ):
                with self.subTest(restart_boundary=boundary, download=name):
                    parent = path.parent
                    retained_parent = identity(parent)
                    self.signatures.reads.clear()
                    self.containers.reads.clear()

                    def make_download_public():
                        parent.chmod(0o777)

                    if boundary == "signature":
                        self.signatures.on_read = make_download_public
                    else:
                        self.containers.on_read = make_download_public
                    try:
                        decision = self.planner.start(vm)
                        self.assertEqual(parent.stat().st_mode & 0o777, 0o777)
                        self.assertEqual(identity(parent), retained_parent)
                        self.assert_files_unchanged(files)
                        self.assert_files_unchanged(vm.ownership.credential_files)
                        self.assertEqual(len(self.signatures.reads), 1)
                        if boundary == "container":
                            self.assertEqual(self.containers.reads, [CONTAINER_ID])
                        self.assertTrue(vm.ownership.manifest.path.is_file())
                        self.assertIn(
                            decision.classification,
                            (Classification.UNSAFE_PATH, Classification.UNVERIFIED_IMAGE),
                        )
                        self.assertIsNone(decision.plan)
                    finally:
                        parent.chmod(0o700)
                        self.signatures.on_read = None
                        self.containers.on_read = None

        def change_image():
            vm.request.image.path.write_bytes(b"changed image during container inspection")

        self.containers.on_read = change_image
        self.assert_denied(self.planner.start(vm), Classification.UNVERIFIED_IMAGE)

    def test_start_rejects_download_checkout_marker_created_during_container_inspection(self):
        download_root = self.root / "downloads-only"
        download_root.mkdir(mode=0o700)
        original_image = self.request.image
        for path in (original_image.path, original_image.checksums, original_image.signature):
            path.parent.rename(download_root / path.parent.name)
        image = replace(
            original_image,
            path=download_root / original_image.path.parent.name / original_image.path.name,
            checksums=download_root
            / original_image.checksums.parent.name
            / original_image.checksums.name,
            signature=download_root
            / original_image.signature.parent.name
            / original_image.signature.name,
        )
        self.request = replace(self.request, image=image)
        self.assertFalse(download_root.is_relative_to(self.checkout))
        self.assertFalse(self.directory.is_relative_to(download_root))
        self.assertFalse(self.checkout.is_relative_to(download_root))
        vm = self.prepared()
        configuration = ContainerConfiguration(
            runner_image=RUNNER_IMAGE,
            runner=self.runner,
            cpus=2,
            memory_mib=6144,
            memory_swap_mib=6144,
            pids_limit=256,
            read_only=True,
            privileged=False,
            cap_drop=("ALL",),
            cap_add=(),
            security_options=("no-new-privileges:true",),
            supplemental_groups=(self.runner.kvm_gid,),
            tmpfs=("/tmp:rw,noexec,nosuid,size=64m",),
            devices=("/dev/kvm:/dev/kvm:rw",),
            bind_mounts=((self.directory, "/vm"),),
            volume_mounts=(),
            namespaces=("bridge", "", "private", "", ""),
            publications=("127.0.0.1:25022:2222/tcp",),
            publish_all=False,
            log_driver="local",
            log_options=("max-size=10m", "max-file=2"),
            entrypoint=("/usr/bin/qemu-system-x86_64",),
            arguments=(
                "-nodefaults",
                "-no-user-config",
                "-display",
                "none",
                "-monitor",
                "none",
                "-accel",
                "kvm",
                "-machine",
                "q35",
                "-cpu",
                "host",
                "-smp",
                "2",
                "-m",
                "4096",
                "-drive",
                "file=/vm/os.qcow2,format=qcow2,if=virtio",
                "-drive",
                "file=/vm/encrypted.qcow2,format=qcow2,if=virtio",
                "-drive",
                "file=/vm/seed.iso,format=raw,if=ide,media=cdrom,readonly=on",
                "-netdev",
                "user,id=net0,hostfwd=tcp:0.0.0.0:2222-:22",
                "-device",
                "virtio-net-pci,netdev=net0",
                "-qmp",
                "unix:/vm/qmp.sock,server=on,wait=off",
                "-serial",
                "stdio",
            ),
        )
        self.containers.observation = ContainerObservation(
            CONTAINER_ID,
            TOKEN,
            self.directory,
            running=False,
            configuration=configuration,
            policy=bounded_policy(),
        )
        plan = self.assert_planned(self.planner.start(vm), Phase.START)
        self.assertEqual(plan.commands, (("docker", "start", CONTAINER_ID),))
        self.signatures.reads.clear()
        self.containers.reads.clear()
        paths = (
            self.checkout,
            self.directory,
            vm.ownership.manifest.path,
            vm.ownership.credential_directory.path,
            *(owned.path for owned in vm.ownership.credential_files),
            image.path.parent,
            image.checksums.parent,
            image.signature.parent,
            image.path,
            image.checksums,
            image.signature,
        )
        retained = tuple(identity(path) for path in paths)
        stamps = tuple((path, path.stat()) for path in paths)
        ancestor = identity(download_root)
        signed_pair = (image.checksums.read_bytes(), image.signature.read_bytes())
        marker = download_root / ".git"
        self.assertFalse(marker.exists())

        def create_checkout_marker() -> None:
            marker.touch(mode=0o600)

        self.containers.on_read = create_checkout_marker
        decision = self.planner.start(vm)
        # The VM stays outside checkouts; only the download ancestry gains a marker.
        self.assertTrue(marker.is_file())
        self.assertEqual(identity(download_root), ancestor)
        self.assert_files_unchanged(retained)
        self.assert_stamps_unchanged(stamps)
        self.assertEqual(self.signatures.reads, [signed_pair])
        self.assertEqual(self.containers.reads, [CONTAINER_ID])
        self.containers.on_read = None
        self.assert_denied(self.planner.start(vm), Classification.UNSAFE_PATH)
        self.assertEqual(self.signatures.reads, [signed_pair])
        self.assertEqual(self.containers.reads, [CONTAINER_ID])
        self.assertEqual(decision.phase, Phase.START)
        self.assertIn(
            decision.classification,
            (Classification.UNSAFE_PATH, Classification.UNVERIFIED_IMAGE),
        )
        self.assertIsNone(decision.plan)

    def test_prepare_admits_an_exact_local_runner_id_with_an_independent_matching_pin(self):
        request = replace(self.request, runner_image=LOCAL_RUNNER_IMAGE)
        planner = VmPlanner(
            self.checkout,
            self.signatures,
            self.containers,
            self.runner,
            runner_image_pin=LOCAL_RUNNER_IMAGE,
        )
        plan = self.assert_planned(planner.prepare(request), Phase.PREPARE)
        self.assertIsNotNone(plan.image_evidence)
        if plan.image_evidence is not None:
            self.assertEqual(plan.image_evidence.runner_image, LOCAL_RUNNER_IMAGE)
        self.assertEqual(list(self.directory.iterdir()), [])
        self.assertEqual(self.containers.reads, [])

    def test_local_runner_ids_cannot_supply_their_own_authority_or_adopt_tags(self):
        for requested, pin in (
            (LOCAL_RUNNER_IMAGE, None),
            (LOCAL_RUNNER_IMAGE, "sha256:" + "e" * 64),
            ("qemu:owned", LOCAL_RUNNER_IMAGE),
            ("sha256:" + "A" * 64, "sha256:" + "A" * 64),
            ("sha256:" + "d" * 63, "sha256:" + "d" * 63),
            ("sha256:" + "d" * 65, "sha256:" + "d" * 65),
            (LOCAL_RUNNER_IMAGE + "\n", LOCAL_RUNNER_IMAGE + "\n"),
        ):
            with self.subTest(case=(requested, pin)):
                planner = VmPlanner(
                    self.checkout,
                    self.signatures,
                    self.containers,
                    self.runner,
                    runner_image_pin=pin,
                )
                request = replace(self.request, runner_image=requested)
                self.assert_denied(planner.prepare(request), Classification.UNVERIFIED_IMAGE)
        self.assertEqual(self.signatures.reads, [])
        self.assertEqual(self.containers.reads, [])

    def test_prepare_retains_the_independently_pinned_exact_guest_byte_size(self):
        plan = self.assert_planned(self.planner.prepare(self.request), Phase.PREPARE)
        if plan.image_evidence is None:
            self.fail("admission must retain authenticated image evidence")
        self.assertEqual(plan.image_evidence.size_bytes, self.request.image.size_bytes)
        self.assertEqual(plan.image_evidence.size_bytes, len(self.request.image.path.read_bytes()))
        self.assertEqual(
            self.signatures.reads,
            [
                (
                    self.request.image.checksums.read_bytes(),
                    self.request.image.signature.read_bytes(),
                )
            ],
        )

    def test_prepare_rejects_missing_untyped_out_of_bound_and_nonmatching_guest_size_pins(self):
        size = len(self.request.image.path.read_bytes())
        for pin in (None, True, 0, -1, str(size), 8 * 1024**3 + 1, size - 1, size + 1):
            with self.subTest(size_pin=pin):
                request = replace(self.request, image=replace(self.request.image, size_bytes=pin))
                decision = self.planner.prepare(request)
                self.assert_denied(decision, Classification.UNVERIFIED_IMAGE)
                self.assertEqual(list(self.directory.iterdir()), [])
        self.assertEqual(self.containers.reads, [])

    def test_start_proposes_both_disks_and_an_explicit_read_only_nocloud_seed(self):
        vm = self.prepared(container_id=None)
        plan = self.assert_planned(self.planner.start(vm), Phase.START)
        command = plan.commands[0]
        drives = [
            command[index + 1] for index, value in enumerate(command[:-1]) if value == "-drive"
        ]
        self.assertEqual(
            drives,
            [
                "file=/vm/os.qcow2,format=qcow2,if=virtio",
                "file=/vm/encrypted.qcow2,format=qcow2,if=virtio",
                "file=/vm/seed.iso,format=raw,if=ide,media=cdrom,readonly=on",
            ],
        )

    def test_start_requires_existing_retained_disks_and_a_bounded_cidata_seed(self):
        vm = self.prepared(container_id=None)
        if vm.artifacts is None:
            self.fail("fixture must retain real disk and seed identities")
        artifacts = vm.artifacts
        for modified, failure in (
            (None, Classification.INVALID_STATE),
            (
                replace(
                    artifacts,
                    os_disk=replace(
                        artifacts.os_disk,
                        file=replace(
                            artifacts.os_disk.file, path=self.directory / "uncreated.qcow2"
                        ),
                    ),
                ),
                Classification.OWNERSHIP_MISMATCH,
            ),
            (
                replace(
                    artifacts,
                    seed=replace(
                        artifacts.seed,
                        file=replace(artifacts.seed.file, path=self.directory / "uncreated.iso"),
                    ),
                ),
                Classification.OWNERSHIP_MISMATCH,
            ),
            (
                replace(artifacts, seed=replace(artifacts.seed, label="OTHER")),
                Classification.INVALID_STATE,
            ),
        ):
            with self.subTest(failure=failure.value, artifacts=modified):
                self.assert_denied(self.planner.start(replace(vm, artifacts=modified)), failure)
        seed = artifacts.seed.file.path
        data = b"x" * (4 * 1024**2 + 1)
        seed.write_bytes(data)
        oversized = SeedEvidence(
            identity(seed), len(data), hashlib.sha256(data).hexdigest(), "CIDATA"
        )
        self.assert_denied(
            self.planner.start(replace(vm, artifacts=replace(artifacts, seed=oversized))),
            Classification.INVALID_RESOURCES,
        )
        self.assertEqual(self.containers.reads, [])
        self.assertTrue(vm.ownership.manifest.path.is_file())

    def test_start_requires_content_pins_for_both_large_prepared_disks(self):
        vm = self.prepared(container_id=None)
        if vm.artifacts is None:
            self.fail("fixture must retain independently observed disk identities")
        disks = {}
        for name in ("os_disk", "encrypted_disk"):
            disk = getattr(vm.artifacts, name)
            data = b"allocated synthetic disk bytes\n" * (8 * 1024)
            disk.file.path.write_bytes(data)
            self.assertGreater(len(data), 64 * 1024)
            retained = replace(
                identity(disk.file.path), content_sha256=hashlib.sha256(data).hexdigest()
            )
            disks[name] = replace(
                disk, file=retained, allocated_bytes=disk.file.path.stat().st_blocks * 512
            )
        vm = replace(vm, artifacts=replace(vm.artifacts, **disks))
        assert vm.artifacts is not None
        self.write_manifest(vm)
        self.assert_planned(self.planner.start(vm), Phase.START)

        for name, disk in disks.items():
            with self.subTest(unpinned_disk=name):
                unpinned = replace(disk, file=replace(disk.file, content_sha256=None))
                changed = replace(vm, artifacts=replace(vm.artifacts, **{name: unpinned}))
                self.assert_denied(self.planner.start(changed), Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.containers.reads, [])

    def test_restart_rejects_missing_or_dangerous_effective_runner_policy(self):
        vm = self.prepared()
        configuration = bounded_configuration(self.directory, self.runner)
        policy = bounded_policy()
        observation = ContainerObservation(
            CONTAINER_ID, TOKEN, self.directory, False, configuration, policy
        )
        self.containers.observation = observation
        self.assert_planned(self.planner.start(vm), Phase.START)
        dangerous = (
            ("environment", ("LD_PRELOAD=/vm/foreign.so",)),
            ("healthcheck", ("CMD-SHELL", "foreign-command")),
            ("declared_volumes", ("/foreign",)),
            ("declared_ports", ("8080/tcp",)),
            ("working_directory", "/foreign"),
            ("restart_policy", "always"),
            ("restart_retries", 1),
            ("auto_remove", True),
            ("runtime", "foreign-runtime"),
            ("device_requests", ("gpu:all",)),
            ("device_cgroup_rules", ("c *:* rwm",)),
            ("volumes_from", ("foreign-container",)),
            ("bind_propagation", "rshared"),
            ("recursive_submounts", True),
            ("cgroup_namespace", "host"),
            ("seccomp_profile", "unconfined"),
            ("apparmor_profile", "unconfined"),
            ("resource_overrides", ("cpuset-cpus=0",)),
            ("resource_overrides", ("cpu-quota=-1",)),
            ("resource_overrides", ("cpu-period=1",)),
            ("resource_overrides", ("cpu-shares=1024",)),
            ("resource_overrides", ("memory-reservation=1",)),
            ("resource_overrides", ("memory-swappiness=100",)),
            ("resource_overrides", ("blkio-weight=1000",)),
            ("resource_overrides", ("ulimit-nproc=unlimited",)),
            ("oom_kill_disable", True),
            ("oom_score_adj", -1000),
            ("sysctls", ("net.ipv4.ip_forward=1",)),
            ("cgroup_parent", "foreign"),
            ("storage_options", ("size=unbounded",)),
            ("stop_signal", "SIGKILL"),
            ("stop_timeout_seconds", 0),
        )
        for name, value in (("missing", None), *dangerous):
            with self.subTest(policy_field=name, value=value):
                modified = None if name == "missing" else replace(policy, **{name: value})
                self.containers.observation = replace(observation, policy=modified)
                self.assert_denied(self.planner.start(vm), Classification.OWNERSHIP_MISMATCH)
        for name, value in (
            ("cpus", 3),
            ("memory_mib", 8192),
            ("memory_swap_mib", 8192),
            ("pids_limit", -1),
            ("read_only", False),
            ("privileged", True),
            ("cap_add", ("SYS_ADMIN",)),
            ("security_options", ("seccomp=unconfined",)),
            ("devices", ("/dev/kvm:/dev/kvm:rw", "/dev/sda:/dev/sda:rw")),
            ("namespaces", ("host", "", "private", "", "")),
            ("namespaces", ("bridge", "host", "private", "", "")),
            ("namespaces", ("bridge", "", "host", "", "")),
            ("namespaces", ("bridge", "", "private", "host", "")),
            ("namespaces", ("bridge", "", "private", "", "host")),
            ("publications", ("127.0.0.1:25022:2222/tcp", "[::]:25022:2222/tcp")),
            ("publications", ("127.0.0.1:25022:2222/udp",)),
            ("publish_all", True),
        ):
            with self.subTest(configuration_field=name, value=value):
                self.containers.observation = replace(
                    observation, configuration=replace(configuration, **{name: value})
                )
                self.assert_denied(self.planner.start(vm), Classification.OWNERSHIP_MISMATCH)

    def test_repository_runner_reference_cannot_supply_the_actual_local_image_id(self):
        vm = self.prepared()
        self.containers.observation = ContainerObservation(
            CONTAINER_ID,
            TOKEN,
            self.directory,
            False,
            bounded_configuration(self.directory, self.runner),
            bounded_policy(),
        )
        unpinned_object = VmPlanner(
            self.checkout,
            self.signatures,
            self.containers,
            self.runner,
            runner_image_pin=RUNNER_IMAGE,
        )
        self.assert_denied(unpinned_object.start(vm), Classification.UNVERIFIED_IMAGE)
        self.assert_planned(self.planner.start(vm), Phase.START)
        observation = self.containers.observation
        if observation is None or observation.policy is None:
            self.fail("fixture must retain the independently expected actual image ID")
        self.containers.observation = replace(
            observation, policy=replace(observation.policy, image_id="sha256:" + "e" * 64)
        )
        self.assert_denied(self.planner.start(vm), Classification.OWNERSHIP_MISMATCH)

    def test_cli_errors_and_execution_gate_are_content_free(self):
        parser = VmArgumentParser(Phase.PREPARE, add_help=False)
        parser.add_argument("--cpus", type=int)
        output = io.StringIO()
        with redirect_stderr(output), self.assertRaises(SystemExit) as failure:
            parser.parse_args(["--cpus", "sensitive-input /private/example"])
        self.assertEqual(failure.exception.code, 2)
        self.assertEqual(output.getvalue(), "phase=prepare classification=invalid-request\n")
        for phase in Phase:
            output = io.StringIO()
            with redirect_stdout(output):
                status = run_task(phase, request=self.request, receipt=self.root / "absent-receipt")
            self.assertEqual(status, 2)
            self.assertEqual(
                output.getvalue(), f"phase={phase.value} classification=gate-not-ready\n"
            )
        self.assertEqual(self.signatures.reads, [])
        self.assertEqual(self.containers.reads, [])
        self.assertEqual(list(self.directory.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
