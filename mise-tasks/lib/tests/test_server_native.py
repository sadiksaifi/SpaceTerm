"""Public-interface tests for read-only native feasibility and retained installation state."""

import io
import unittest
from contextlib import redirect_stderr, redirect_stdout
from dataclasses import asdict
from typing import cast
from unittest.mock import patch

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.server_native import (
    Classification,
    DesiredState,
    Evidence,
    HostFacts,
    HostPlatform,
    Namespace,
    NativeFeasibility,
    NativeObservation,
    Phase,
    Registration,
    Storage,
    Supervisor,
    Version,
)


class ControlledNativeInspector:
    """The native host is controlled; the feasibility module and its evidence remain real."""

    def __init__(self, observation: NativeObservation, error: BaseException | None = None):
        self.observation = observation
        self.error = error
        self.reads: list[Namespace] = []
        self.mutations: list[str] = []

    def observe(self, namespace: Namespace) -> NativeObservation:
        self.reads.append(namespace)
        if self.error is not None:
            raise self.error
        return self.observation

    def install(self):
        self.mutations.append("install")

    def start_server(self):
        self.mutations.append("start-server")

    def load_credentials(self):
        self.mutations.append("load-credentials")

    def unlock_storage(self):
        self.mutations.append("unlock-storage")

    def initialize_storage(self):
        self.mutations.append("initialize-storage")

    def change_privacy(self):
        self.mutations.append("change-privacy")


class NativeFeasibilityTests(unittest.TestCase):
    def observation(
        self,
        registration: Registration = Registration.PRESENT,
        desired_state: DesiredState = DesiredState.RUNNING,
        storage: Storage = Storage.AVAILABLE,
        host: HostFacts | None = None,
    ) -> NativeObservation:
        if host is None:
            host = HostFacts(
                platform=HostPlatform.MACOS,
                os_version=Version(26, 0, 1),
                supervisor=Supervisor.LAUNCHD,
                credential_storage=Storage.AVAILABLE,
            )
        return NativeObservation(
            registration=registration,
            desired_state=desired_state,
            application_storage=storage,
            host=host,
        )

    def test_default_inspection_observes_without_host_integration_changes(self):
        observation = self.observation()
        native = ControlledNativeInspector(observation)
        # Native commands must stay in the injected adapter, never in the portable policy.
        with (
            patch("subprocess.run", side_effect=AssertionError("native command escaped adapter")),
            patch("subprocess.Popen", side_effect=AssertionError("native process escaped adapter")),
        ):
            evidence = NativeFeasibility(native).run()
        self.assertEqual(native.mutations, [])
        self.assertEqual(native.reads, [Namespace.DEVELOPMENT])
        self.assertEqual(evidence.phase, Phase.INSPECT)
        self.assertEqual(evidence.classification, Classification.READY)
        self.assertEqual(evidence.observation, observation)

    def test_stopped_registration_takes_precedence_over_storage_and_host_readiness(self):
        for storage, host in (
            (Storage.LOCKED, HostFacts(credential_storage=Storage.AVAILABLE)),
            (Storage.DENIED, HostFacts(credential_storage=Storage.DENIED)),
            (Storage.UNAVAILABLE, HostFacts(credential_storage=Storage.LOCKED)),
            (
                Storage.UNCHECKED,
                HostFacts(platform=HostPlatform.UNSUPPORTED, supervisor=Supervisor.MISSING),
            ),
        ):
            with self.subTest(storage=storage, host=host):
                observation = self.observation(
                    desired_state=DesiredState.STOPPED,
                    storage=storage,
                    host=host,
                )
                native = ControlledNativeInspector(observation)
                evidence = NativeFeasibility(native).run()
                self.assertEqual(native.mutations, [])
                self.assertEqual(evidence.classification, Classification.STOPPED)
                self.assertEqual(evidence.observation, observation)
                self.assertEqual(native.observation, observation)

    def test_locked_storage_preserves_present_registration_without_reinitialization(self):
        for storage, credential_storage in (
            (Storage.LOCKED, Storage.AVAILABLE),
            (Storage.AVAILABLE, Storage.LOCKED),
        ):
            with self.subTest(storage=storage, credential_storage=credential_storage):
                observation = self.observation(
                    storage=storage,
                    host=HostFacts(
                        platform=HostPlatform.MACOS,
                        supervisor=Supervisor.LAUNCHD,
                        credential_storage=credential_storage,
                    ),
                )
                native = ControlledNativeInspector(observation)
                evidence = NativeFeasibility(native).run()
                self.assertEqual(native.mutations, [])
                self.assertEqual(evidence.classification, Classification.STORAGE_LOCKED)
                self.assertEqual(evidence.observation, observation)
                self.assertEqual(native.observation, observation)

    def test_unavailable_storage_preserves_present_registration_without_reinitialization(self):
        for storage, credential_storage in (
            (Storage.UNAVAILABLE, Storage.AVAILABLE),
            (Storage.AVAILABLE, Storage.UNAVAILABLE),
        ):
            with self.subTest(storage=storage, credential_storage=credential_storage):
                observation = self.observation(
                    storage=storage,
                    host=HostFacts(
                        platform=HostPlatform.MACOS,
                        supervisor=Supervisor.LAUNCHD,
                        credential_storage=credential_storage,
                    ),
                )
                native = ControlledNativeInspector(observation)
                evidence = NativeFeasibility(native).run()
                self.assertEqual(native.mutations, [])
                self.assertEqual(evidence.classification, Classification.STORAGE_UNAVAILABLE)
                self.assertEqual(evidence.observation, observation)
                self.assertEqual(native.observation, observation)

    def test_storage_denial_is_not_a_missing_installation(self):
        for storage, credential_storage in (
            (Storage.DENIED, Storage.AVAILABLE),
            (Storage.AVAILABLE, Storage.DENIED),
        ):
            with self.subTest(storage=storage, credential_storage=credential_storage):
                observation = self.observation(
                    storage=storage,
                    host=HostFacts(
                        platform=HostPlatform.MACOS,
                        supervisor=Supervisor.LAUNCHD,
                        credential_storage=credential_storage,
                    ),
                )
                native = ControlledNativeInspector(observation)
                evidence = NativeFeasibility(native).run()
                self.assertEqual(evidence.classification, Classification.INSPECTION_DENIED)
                self.assertEqual(evidence.observation, observation)
                self.assertEqual(native.mutations, [])

    def test_missing_registration_is_distinct_from_denied_or_unavailable_inspection(self):
        for registration, classification in (
            (Registration.MISSING, Classification.MISSING_INSTALLATION),
            (Registration.DENIED, Classification.INSPECTION_DENIED),
            (Registration.UNAVAILABLE, Classification.INSPECTION_UNAVAILABLE),
            (Registration.UNCHECKED, Classification.GATE_NOT_READY),
        ):
            with self.subTest(registration=registration):
                observation = self.observation(
                    registration=registration,
                    desired_state=DesiredState.UNKNOWN,
                    storage=Storage.UNCHECKED,
                )
                native = ControlledNativeInspector(observation)
                evidence = NativeFeasibility(native).run()
                self.assertEqual(native.mutations, [])
                self.assertEqual(evidence.classification, classification)
                self.assertEqual(evidence.observation, observation)

    def test_native_permission_failure_is_a_content_free_denial_not_a_missing_installation(self):
        sensitive = "/private/example/native-error-with-credential-and-terminal-content"
        native = ControlledNativeInspector(self.observation(), PermissionError(13, sensitive))
        output = io.StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            evidence = NativeFeasibility(native).run()
        self.assertEqual(native.mutations, [])
        self.assertEqual(evidence.classification, Classification.INSPECTION_DENIED)
        self.assertIsNone(evidence.observation)
        self.assertEqual(output.getvalue(), "")
        self.assertNotIn(sensitive, repr(evidence))

    def test_native_storage_read_failure_is_unavailable_not_missing(self):
        sensitive = "/private/example/native-error-with-credential-and-terminal-content"
        for error in (OSError(5, sensitive), FileNotFoundError(2, sensitive, sensitive)):
            with self.subTest(error_type=type(error)):
                native = ControlledNativeInspector(self.observation(), error)
                output = io.StringIO()
                with redirect_stdout(output), redirect_stderr(output):
                    evidence = NativeFeasibility(native).run()
                self.assertEqual(native.mutations, [])
                self.assertEqual(evidence.classification, Classification.INSPECTION_UNAVAILABLE)
                self.assertIsNone(evidence.observation)
                self.assertEqual(output.getvalue(), "")
                self.assertNotIn(sensitive, repr(evidence))

    def test_native_runtime_error_returns_content_free_unavailable_evidence(self):
        sensitive = "SYNTHETIC_NATIVE_RUNTIME_ERROR_PATH_CREDENTIAL_TERMINAL_CONTENT"
        native = ControlledNativeInspector(self.observation(), RuntimeError(sensitive))
        output = io.StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            evidence = NativeFeasibility(native).run()
        self.assertEqual(native.reads, [Namespace.DEVELOPMENT])
        self.assertEqual(native.mutations, [])
        self.assertEqual(output.getvalue(), "")
        self.assertEqual(
            asdict(evidence),
            {
                "phase": Phase.INSPECT,
                "namespace": Namespace.DEVELOPMENT,
                "classification": Classification.INSPECTION_UNAVAILABLE,
                "observation": None,
            },
        )
        self.assertNotIn(sensitive, repr(evidence))

    def test_native_task_error_returns_content_free_evidence_through_shared_main(self):
        sensitive = "SYNTHETIC_NATIVE_TASK_ERROR_PATH_CREDENTIAL_TERMINAL_CONTENT"
        native = ControlledNativeInspector(self.observation(), TaskError(sensitive))
        results: list[Evidence] = []

        def entry():
            results.append(NativeFeasibility(native).run())
            return 2

        output = io.StringIO()
        with (
            redirect_stdout(output),
            redirect_stderr(output),
            self.assertRaises(SystemExit) as exited,
        ):
            main(entry)
        self.assertEqual(output.getvalue(), "")
        self.assertEqual(exited.exception.code, 2)
        self.assertIsNone(exited.exception.__cause__)
        self.assertIsNone(exited.exception.__context__)
        self.assertEqual(native.reads, [Namespace.DEVELOPMENT])
        self.assertEqual(native.mutations, [])
        self.assertEqual(len(results), 1)
        self.assertEqual(
            asdict(results[0]),
            {
                "phase": Phase.INSPECT,
                "namespace": Namespace.DEVELOPMENT,
                "classification": Classification.INSPECTION_UNAVAILABLE,
                "observation": None,
            },
        )
        self.assertNotIn(sensitive, repr(results[0]))

    def test_native_inspection_preserves_keyboard_interrupt_and_system_exit(self):
        for error in (KeyboardInterrupt(), SystemExit(73)):
            with self.subTest(error_type=type(error)):
                native = ControlledNativeInspector(self.observation(), error)
                output = io.StringIO()
                with (
                    redirect_stdout(output),
                    redirect_stderr(output),
                    self.assertRaises(type(error)) as raised,
                ):
                    NativeFeasibility(native).run()
                self.assertIs(raised.exception, error)
                self.assertEqual(native.reads, [Namespace.DEVELOPMENT])
                self.assertEqual(native.mutations, [])
                self.assertEqual(output.getvalue(), "")

    def test_phases_without_adapters_do_not_inspect_or_claim_native_evidence(self):
        evidence = NativeFeasibility().run()
        self.assertEqual(evidence.classification, Classification.GATE_NOT_READY)
        self.assertIsNone(evidence.observation)

        native = ControlledNativeInspector(self.observation())
        for phase in (Phase.PREPARE, Phase.INSTALL, Phase.EXERCISE, Phase.CLEANUP):
            with self.subTest(phase=phase):
                evidence = NativeFeasibility(native).run(phase)
                self.assertEqual(evidence.classification, Classification.GATE_NOT_READY)
                self.assertIsNone(evidence.observation)
        self.assertEqual(native.reads, [])
        self.assertEqual(native.mutations, [])

    def test_invalid_phase_is_rejected_before_native_inspection(self):
        native = ControlledNativeInspector(self.observation())
        for phase in ("inspect", "install", None, True):
            with self.subTest(phase=phase):
                with self.assertRaisesRegex(TaskError, "^invalid-evidence$"):
                    NativeFeasibility(native).run(cast(Phase, phase))
        self.assertEqual(native.reads, [])
        self.assertEqual(native.mutations, [])

    def test_running_registration_requires_supported_host_and_observed_prerequisites(self):
        for storage, host, classification in (
            (Storage.UNCHECKED, self.observation().host, Classification.GATE_NOT_READY),
            (Storage.AVAILABLE, HostFacts(), Classification.GATE_NOT_READY),
            (
                Storage.AVAILABLE,
                HostFacts(platform=HostPlatform.MACOS, credential_storage=Storage.AVAILABLE),
                Classification.GATE_NOT_READY,
            ),
            (
                Storage.AVAILABLE,
                HostFacts(platform=HostPlatform.MACOS, supervisor=Supervisor.LAUNCHD),
                Classification.GATE_NOT_READY,
            ),
            (
                Storage.AVAILABLE,
                HostFacts(platform=HostPlatform.UNSUPPORTED),
                Classification.UNSUPPORTED_HOST,
            ),
            (
                Storage.AVAILABLE,
                HostFacts(platform=HostPlatform.LINUX, supervisor=Supervisor.MISSING),
                Classification.UNSUPPORTED_HOST,
            ),
            (
                Storage.AVAILABLE,
                HostFacts(
                    platform=HostPlatform.LINUX,
                    supervisor=Supervisor.SYSTEMD,
                    credential_storage=Storage.AVAILABLE,
                ),
                Classification.GATE_NOT_READY,
            ),
            (
                Storage.AVAILABLE,
                HostFacts(
                    platform=HostPlatform.LINUX,
                    supervisor=Supervisor.SYSTEMD,
                    supervisor_version=Version(248),
                    credential_storage=Storage.AVAILABLE,
                ),
                Classification.UNSUPPORTED_HOST,
            ),
            (
                Storage.AVAILABLE,
                HostFacts(
                    platform=HostPlatform.LINUX,
                    supervisor=Supervisor.SYSTEMD,
                    supervisor_version=Version(249),
                    credential_storage=Storage.AVAILABLE,
                ),
                Classification.READY,
            ),
        ):
            with self.subTest(storage=storage, host=host):
                observation = self.observation(storage=storage, host=host)
                native = ControlledNativeInspector(observation)
                evidence = NativeFeasibility(native).run()
                self.assertEqual(evidence.classification, classification)
                self.assertEqual(evidence.observation, observation)
                self.assertEqual(native.mutations, [])

    def test_invalid_adapter_result_is_rejected_without_exposing_its_contents(self):
        native = ControlledNativeInspector(self.observation())
        for result in (None, {"registration": "missing", "native_message": "/private/example"}):
            with self.subTest(result_type=type(result)):
                output = io.StringIO()
                with (
                    patch.object(native, "observe", return_value=result),
                    redirect_stdout(output),
                    redirect_stderr(output),
                    self.assertRaisesRegex(TaskError, "^invalid-evidence$"),
                ):
                    NativeFeasibility(native).run()
                self.assertEqual(output.getvalue(), "")
                self.assertEqual(native.mutations, [])

    def test_registration_state_and_storage_must_describe_the_same_installation(self):
        cases: list[tuple[Registration, DesiredState, Storage]] = [
            (Registration.PRESENT, DesiredState.UNKNOWN, Storage.AVAILABLE)
        ]
        for registration in (
            Registration.MISSING,
            Registration.DENIED,
            Registration.UNAVAILABLE,
            Registration.UNCHECKED,
        ):
            cases.extend(
                (
                    (registration, DesiredState.RUNNING, Storage.UNCHECKED),
                    (registration, DesiredState.STOPPED, Storage.UNCHECKED),
                    (registration, DesiredState.UNKNOWN, Storage.AVAILABLE),
                    (registration, DesiredState.UNKNOWN, Storage.LOCKED),
                )
            )
        for registration, desired_state, storage in cases:
            with self.subTest(
                registration=registration, desired_state=desired_state, storage=storage
            ):
                with self.assertRaisesRegex(TaskError, "^invalid-evidence$"):
                    self.observation(registration, desired_state, storage)

    def test_supervisor_facts_cannot_contradict_the_platform_or_presence(self):
        for platform, supervisor, version in (
            (HostPlatform.MACOS, Supervisor.SYSTEMD, None),
            (HostPlatform.LINUX, Supervisor.LAUNCHD, None),
            (HostPlatform.MACOS, Supervisor.MISSING, Version(1)),
            (HostPlatform.LINUX, Supervisor.UNKNOWN, Version(249)),
        ):
            with self.subTest(platform=platform, supervisor=supervisor, version=version):
                with self.assertRaisesRegex(TaskError, "^invalid-evidence$"):
                    HostFacts(
                        platform=platform,
                        supervisor=supervisor,
                        supervisor_version=version,
                    )


class ReadOnlyMacOSInspectorTests(unittest.TestCase):
    def test_default_inspection_never_installs_unlocks_or_prompts(self):
        from spaceterm_tasks.server_native import MacOSReadOnlyInspector

        class RegistrationReader:
            def read(self, namespace, uid):
                return None

        with (
            patch("subprocess.Popen", side_effect=AssertionError("native command")),
            patch("subprocess.run", side_effect=AssertionError("native command")),
        ):
            inspector = MacOSReadOnlyInspector(
                RegistrationReader(),
                HostFacts(
                    platform=HostPlatform.MACOS,
                    os_version=Version(26),
                    supervisor=Supervisor.LAUNCHD,
                ),
                501,
            )
            evidence = NativeFeasibility(inspector).run()
        self.assertEqual(evidence.classification, Classification.MISSING_INSTALLATION)
        assert evidence.observation is not None
        self.assertEqual(evidence.observation.host.credential_storage, Storage.UNCHECKED)

    def test_read_only_inspector_preserves_stopped_without_storage_probe(self):
        from spaceterm_tasks.server_native import MacOSReadOnlyInspector

        class RegistrationReader:
            def read(self, namespace, uid):
                return (
                    b"STRGREC\0"
                    + bytes((1, 0, 3, 2))
                    + (501).to_bytes(4, "little")
                    + (1).to_bytes(8, "little") * 2
                    + bytes(16)
                )

        evidence = NativeFeasibility(
            MacOSReadOnlyInspector(RegistrationReader(), HostFacts(), 501)
        ).run()
        self.assertEqual(evidence.classification, Classification.STOPPED)
        assert evidence.observation is not None
        self.assertEqual(evidence.observation.application_storage, Storage.UNCHECKED)

    def test_read_only_inspector_does_not_admit_malformed_or_new_records(self):
        from spaceterm_tasks.server_native import MacOSReadOnlyInspector

        class RegistrationReader:
            def __init__(self, data):
                self.data = data

            def read(self, namespace, uid):
                return self.data

        for data in (bytes(48), b"STRGREC\0" + bytes((2, 0)) + bytes(246), b"raw sensitive error"):
            with self.subTest(size=len(data)):
                evidence = NativeFeasibility(
                    MacOSReadOnlyInspector(RegistrationReader(data), HostFacts(), 501)
                ).run()
                self.assertEqual(evidence.classification, Classification.INSPECTION_UNAVAILABLE)


if __name__ == "__main__":
    unittest.main()
