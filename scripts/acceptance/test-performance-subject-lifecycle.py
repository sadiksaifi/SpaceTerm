#!/usr/bin/env python3

import hashlib, hmac, os, pathlib, stat, struct, subprocess, tempfile, time

HERE = pathlib.Path(__file__).resolve().parent
NONCE = "a" * 64
SECRET = b"0123456789abcdef0123456789abcdef"


def sha(data): return hashlib.sha256(data).hexdigest()
def rows(items): return b"".join(f"{key}\t{value}\n".encode() for key, value in items)
def signed(magic, items, key):
    unsigned = rows(items)
    signature = hmac.new(SECRET, magic + struct.pack(">Q", len(unsigned)) + unsigned,
                         hashlib.sha256).hexdigest()
    return unsigned + f"{key}\t{signature}\n".encode()
def write(path, data, mode=0o400): path.write_bytes(data); path.chmod(mode)


def fixture(root, subject):
    paths = {name: root / f"{subject}-{name}.tsv" for name in (
        "subject", "intent", "events", "metadata", "ready", "gate", "driver-receipt",
        "driver-events", "rss", "trace", "tail", "native", "lifecycle-ready",
        "registration", "quit", "exit")}
    pid = "4242" if subject == "spaceterm" else "4243"
    identity = rows([
        ("format_version","1"),("subject",subject),("app_bundle_path",f"/Applications/{subject}.app"),
        ("bundle_identifier",f"dev.spaceterm.{subject}"),("bundle_version","1+1"),
        ("executable_path",f"/Applications/{subject}.app/Contents/MacOS/{subject}"),
        ("executable_sha256","c"*64),("executable_device","1"),("executable_inode","2"),
        ("executable_fsid","1:1"),("signature_valid","true"),
        ("signing_identifier",f"dev.spaceterm.{subject}"),("team_identifier","none"),
        ("cdhash","abc123"),("process_pid",pid),("process_start_identity","100:200"),
        ("identity_status","frozen")])
    write(paths["subject"], identity)
    intent = rows([("format_version","1"),("subject",subject),
        ("subject_identity_sha256",sha(identity)),("scenario","ascii"),
        ("scenario_plan_sha256","c"*64),("workload_sha256","c"*64),
        ("command_sha256","c"*64),("environment_sha256","c"*64),("font_sha256","c"*64),
        ("initial_grid_sha256","c"*64),("measured_duration_ms","1"),("process_pid",pid),
        ("process_start_identity","100:200"),("campaign_id","campaign-a"),
        ("session_id",f"session-{subject}"),("nonce",NONCE),
        ("native_provisional_observation_sha256","c"*64 if subject == "spaceterm" else "not-applicable"),
        ("evidence_mode","test-only"),
        ("status","prepared")])
    write(paths["intent"], intent)
    prefix = (b"sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\tpixel_width\tpixel_height\tstatus\n"
        b"0\t1000\tseed-complete\tnone\t1\t24\t80\t800\t600\tok\n"
        b"1\t1100\tmeasurement-ready\tnone\t1\t24\t80\t800\t600\tok\n")
    events = prefix + b"2\t3000\tproducer-end\tnone\t1\t24\t80\t800\t600\tsuccess\n"
    write(paths["events"], events)
    event_stat = paths["events"].stat()
    ready_items = [("format_version","1"),("campaign_id","campaign-a"),
        ("session_id",f"session-{subject}"),("nonce",NONCE),("subject_identity_sha256",sha(identity)),
        ("producer_pid","50"),("producer_started_continuous_ns","500"),("producer_session_id","50"),
        ("producer_process_group","50"),("tty_device","1"),("tty_inode","2"),("tty_rdev","3"),
        ("events_device",str(event_stat.st_dev)),("events_inode",str(event_stat.st_ino)),
        ("events_prefix_bytes",str(len(prefix))),("events_prefix_sha256",sha(prefix)),
        ("measurement_ready_continuous_ns","1100"),("measurement_ready_byte_count","1"),
        ("auth_algorithm","hmac-sha256")]
    ready = signed(b"spaceterm.performance.workload-ready/v1\0", ready_items, "ready_hmac_sha256")
    write(paths["ready"], ready)
    metadata_items = [("format_version","3"),("scenario","ascii"),("campaign_id","campaign-a"),
        ("session_id",f"session-{subject}"),("nonce",NONCE),("subject_identity_sha256",sha(identity)),
        ("subject_process_pid",pid),("subject_process_start_identity","100:200"),
        ("producer_sha256","c"*64),("producer_pid","50"),("producer_started_continuous_ns","500"),
        ("producer_session_id","50"),("producer_process_group","50"),("tty_device","1"),
        ("tty_inode","2"),("tty_rdev","3"),("ready_receipt_sha256",sha(ready)),
        ("events_sha256",sha(events)),("auth_algorithm","hmac-sha256"),("seed_sha256","c"*64),
        ("seed_bytes","1"),("requested_duration_ms","1"),("warmup_ms","0"),
        ("requested_iterations","1"),("requested_seed_rows","1"),("emitted_bytes","1"),
        ("input_events","0"),("plan_start_continuous_ns","1000"),("started_continuous_ns","1000"),
        ("ended_continuous_ns","3000"),("status","complete")]
    unsigned = rows(metadata_items)
    metadata_hmac = hmac.new(SECRET, b"spaceterm.performance.workload-auth/v1\0"
        + struct.pack(">Q",len(unsigned)) + unsigned + struct.pack(">Q",len(events)) + events,
        hashlib.sha256).hexdigest()
    metadata = unsigned + f"events_hmac_sha256\t{metadata_hmac}\n".encode()
    write(paths["metadata"], metadata)
    gate = signed(b"spaceterm.performance.plan-start-gate/v1\0", [
        ("format_version","1"),("campaign_id","campaign-a"),("session_id",f"session-{subject}"),
        ("nonce",NONCE),("ready_receipt_sha256",sha(ready)),("plan_start_continuous_ns","1000")],
        "start_gate_hmac_sha256")
    write(paths["gate"], gate)
    trace = signed(b"spaceterm.performance.trace-provisional/v1\0", [
        ("format_version","1"),("subject_identity_sha256",sha(identity)),
        ("run_intent_sha256",sha(intent)),("workload_metadata_sha256",sha(metadata)),
        ("workload_ready_receipt_sha256",sha(ready)),("supplemental_evidence_sha256",sha(gate)),
        ("capture_status","CAPTURED"),("requested_duration_ms","1"),("actual_duration_ms","1"),
        ("capture_started_continuous_ns","1000"),("capture_ended_continuous_ns","3000"),
        ("trace_bundle_tree_sha256","c"*64),("toc_sha256","c"*64),
        ("time_profile_export_sha256","c"*64),("allocations_export_sha256","c"*64),
        ("hangs_export_sha256","c"*64),("trace_verification_sha256","c"*64),
        ("verifier_sha256","c"*64),("evidence_mode","test-only"),
        ("status","complete"),("auth_algorithm","hmac-sha256")],
        "provisional_hmac_sha256")
    write(paths["trace"], trace)
    for key in ("driver-receipt","driver-events","rss"): write(paths[key], key.encode()+b"\n")
    write(paths["native"], b"native-final\n")
    token = ("d" if subject == "spaceterm" else "e") * 64
    fixture_env = os.environ.copy()
    fixture_env["SPACETERM_PERFORMANCE_TEST_MODE"] = "1"
    subprocess.run([HERE/"performance-tail-receipt.py","create","--campaign-secret-file",root/"secret",
        "--campaign-id","campaign-a","--session-id",f"session-{subject}","--nonce",NONCE,
        "--quit-token",token,"--run-intent",paths["intent"],"--subject-identity",paths["subject"],
        "--driver-receipt",paths["driver-receipt"],"--driver-events",paths["driver-events"],
        "--workload-metadata",paths["metadata"],"--workload-events",paths["events"],
        "--workload-ready-receipt",paths["ready"],"--rss-samples",paths["rss"],
        "--trace-provisional-receipt",paths["trace"],
        "--appkit-terminator-source",root/"terminator.m",
        "--appkit-terminator-binary",root/"terminator",
        "--tail-completed-continuous-ns","5000003000",
        "--output",paths["tail"]], check=True, env=fixture_env)
    return paths, token, identity, intent


def run_case(root, subject, *, swap_terminator=False):
    paths, token, identity, intent = fixture(root, subject)
    control = root/f"{subject}-control.fifo"
    command = [HERE/"performance-subject-lifecycle.py","--subject-identity",paths["subject"],
        "--campaign-secret-file",root/"secret","--campaign-id","campaign-a",
        "--session-id",f"session-{subject}","--nonce",NONCE,"--live-ready-receipt",paths["lifecycle-ready"],
        "--registration-control",control,"--quit-receipt",paths["quit"],
        "--subject-exit-receipt",paths["exit"],"--native-observation",
        paths["native"] if subject == "spaceterm" else "not-applicable",
        "--driver-receipt",paths["driver-receipt"],"--driver-events",paths["driver-events"],
        "--workload-metadata",paths["metadata"],"--workload-events",paths["events"],
        "--workload-ready-receipt",paths["ready"],"--rss-samples",paths["rss"],
        "--trace-provisional-receipt",paths["trace"],"--plan-start-gate",paths["gate"],
        "--appkit-terminator-source",root/"terminator.m",
        "--appkit-terminator",root/"terminator",
        "--expected-appkit-terminator-source-device",str((root/"terminator.m").stat().st_dev),
        "--expected-appkit-terminator-source-inode",str((root/"terminator.m").stat().st_ino),
        "--expected-appkit-terminator-source-sha256",sha((root/"terminator.m").read_bytes()),
        "--expected-appkit-terminator-binary-device",str((root/"terminator").stat().st_dev),
        "--expected-appkit-terminator-binary-inode",str((root/"terminator").stat().st_ino),
        "--expected-appkit-terminator-binary-sha256",sha((root/"terminator").read_bytes()),
        "--timeout-seconds","3"]
    env = os.environ.copy(); env.update(SPACETERM_PERFORMANCE_TEST_MODE="1",
        SPACETERM_TEST_LIFECYCLE_IDENTITY="valid",SPACETERM_TEST_LIFECYCLE_TERMINATION="normal")
    process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE)
    deadline=time.time()+3
    while not control.exists() and time.time()<deadline: time.sleep(.01)
    source_stat=(root/"terminator.m").stat(); binary_stat=(root/"terminator").stat()
    source_hash=sha((root/"terminator.m").read_bytes())
    binary_hash=sha((root/"terminator").read_bytes())
    if swap_terminator:
        replacement=root/"terminator.replacement"
        write(replacement,(root/"terminator").read_bytes(),0o500)
        os.replace(replacement,root/"terminator")
    registration_items=[("format_version","1"),("campaign_id","campaign-a"),
        ("session_id",f"session-{subject}"),("nonce",NONCE),("registration_token",token),
        ("subject_identity_sha256",sha(identity)),("process_pid","4242" if subject=="spaceterm" else "4243"),
        ("process_start_identity","100:200"),("run_intent_path",str(paths["intent"])),
        ("run_intent_sha256",sha(intent)),("tail_receipt_path",str(paths["tail"])),
        ("workload_metadata_path",str(paths["metadata"])),("workload_events_path",str(paths["events"])),
        ("workload_ready_receipt_path",str(paths["ready"])),("quit_receipt_path",str(paths["quit"])),
        ("subject_exit_receipt_path",str(paths["exit"])),
        ("native_observation_path",str(paths["native"]) if subject=="spaceterm" else "not-applicable"),
        ("appkit_terminator_source_device",str(source_stat.st_dev)),
        ("appkit_terminator_source_inode",str(source_stat.st_ino)),
        ("appkit_terminator_source_sha256",source_hash),
        ("appkit_terminator_binary_device",str(binary_stat.st_dev)),
        ("appkit_terminator_binary_inode",str(binary_stat.st_ino)),
        ("appkit_terminator_binary_sha256",binary_hash),
        ("evidence_mode","test-only"),
        ("auth_algorithm","hmac-sha256"),("status","registered")]
    unsigned=rows(registration_items)
    signature=hmac.new(SECRET,b"spaceterm.acceptance.performance-lifecycle-registration/v1\0"
        +struct.pack(">Q",len(unsigned))+unsigned,hashlib.sha256).hexdigest()
    contents=rows(registration_items[:-1])+f"registration_hmac_sha256\t{signature}\n".encode()+rows(registration_items[-1:])
    write(paths["registration"],contents)
    with control.open("w") as stream: stream.write(f"register\t{token}\t{paths['registration']}\n")
    _, error=process.communicate(timeout=5); status=process.returncode
    if swap_terminator:
        assert status != 0
        assert b"tail-binding-appkit_terminator_binary_inode" in error \
            or b"terminator-tool-replaced" in error
        assert not paths["quit"].exists() and not paths["exit"].exists()
        return
    assert status==0
    assert paths["quit"].exists() and paths["exit"].exists()
    assert paths["quit"].read_bytes().count(b"status\tcompleted\n")==1
    assert paths["exit"].read_bytes().count(b"status\tcomplete\n")==1
    tool = {
        "appkit_terminator_source_device": str((root/"terminator.m").stat().st_dev),
        "appkit_terminator_source_inode": str((root/"terminator.m").stat().st_ino),
        "appkit_terminator_source_sha256": sha((root/"terminator.m").read_bytes()),
        "appkit_terminator_binary_device": str((root/"terminator").stat().st_dev),
        "appkit_terminator_binary_inode": str((root/"terminator").stat().st_ino),
        "appkit_terminator_binary_sha256": sha((root/"terminator").read_bytes()),
    }
    for artifact in (paths["tail"], paths["quit"], paths["exit"]):
        values=dict(line.split("\t",1) for line in artifact.read_text().splitlines())
        assert all(values[key] == value for key,value in tool.items())


def main():
    with tempfile.TemporaryDirectory(prefix="spaceterm-lifecycle-") as temporary:
        top=pathlib.Path(temporary); top.chmod(0o700)
        for name, subject, swap in (("space","spaceterm",False),("ghost","ghostty",False),
                                    ("swap","ghostty",True)):
            root=top/name; root.mkdir(mode=0o700); write(root/"secret",SECRET,0o600)
            write(root/"terminator.m",b"// frozen source\n",0o400)
            write(root/"terminator",b"fake appkit terminator\n",0o500)
            run_case(root,subject,swap_terminator=swap)
    print("performance subject lifecycle tests passed")

if __name__ == "__main__": main()
