//! Standalone single-threaded composition of the actual guarded executor with
//! a synthetic retained release peer and a fixed descriptor-executed native tool.

use spaceterm_host::{
    boot_credentials::linux::{
        credential_name,
        test_support::{self, Retirement},
    },
    native_deployment::contracts::NativeFailure,
    native_receiver::guard::run_guarded_retriever,
    startup_exchange::{
        ExchangeDeadline,
        unix::{ExactChild, LockedMemory, PipeEndpoint, anonymous_pipe, process_identity},
    },
};
use std::{
    fs::{self, File},
    io::Write,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::Path,
    time::Duration,
};

unsafe extern "C" {
    fn fork() -> i32;
    fn read(fd: i32, data: *mut u8, count: usize) -> isize;
    fn write(fd: i32, data: *const u8, count: usize) -> isize;
    fn _exit(status: i32) -> !;
}

fn encode(bytes: &[u8]) -> Vec<u8> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = Vec::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        encoded.extend_from_slice(&[
            alphabet[a >> 2],
            alphabet[((a & 3) << 4) | (b >> 4)],
            if chunk.len() > 1 {
                alphabet[((b & 15) << 2) | (c >> 6)]
            } else {
                b'='
            },
            if chunk.len() > 2 {
                alphabet[c & 63]
            } else {
                b'='
            },
        ]);
    }
    encoded
}

fn case(
    directory: &Path,
    source: &Path,
    replacement: &Path,
    scenario: u8,
    retirement: Retirement,
) -> bool {
    fs::create_dir(directory).unwrap();
    let selected = directory.join("selected-tool");
    fs::copy(source, &selected).unwrap();
    fs::set_permissions(&selected, fs::Permissions::from_mode(0o700)).unwrap();
    let tool = File::open(&selected).unwrap();
    // Swap the pathname after retention. Reopening it instead of execveat on
    // the original descriptor executes the rejecting binary and fails success.
    let alternate = directory.join("alternate-tool");
    fs::copy(replacement, &alternate).unwrap();
    fs::set_permissions(&alternate, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(&alternate, &selected).unwrap();
    let audit_path = directory.join("audit");
    let audit = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&audit_path)
        .unwrap();
    let name = format!(
        "--name={}",
        credential_name(&test_support::binding()).unwrap()
    );
    let path = audit_path.as_os_str().as_encoded_bytes();
    let mut header = vec![0; 160];
    // Public systemd recipe metadata, not a plaintext credential fixture.
    header[..16].copy_from_slice(&[
        0x5a, 0x1c, 0x6a, 0x86, 0xdf, 0x9d, 0x40, 0x96, 0xb1, 0xd5, 0xa6, 0x5e, 0x08, 0x62, 0xf1,
        0x9a,
    ]);
    for (offset, value) in [(16, 32u32), (20, 1), (24, 12), (28, 16)] {
        header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    if scenario == 7 {
        header[0] ^= 1;
    }
    header[152] = scenario;
    header[154..156].copy_from_slice(&(path.len() as u16).to_le_bytes());
    header[156..158].copy_from_slice(&(name.len() as u16).to_le_bytes());
    header.extend_from_slice(path);
    header.extend_from_slice(name.as_bytes());
    let ciphertext_path = directory.join("ciphertext");
    let mut writer = File::options()
        .write(true)
        .create_new(true)
        .open(&ciphertext_path)
        .unwrap();
    writer.write_all(&encode(&header)).unwrap();
    drop(writer);
    let ciphertext = File::open(&ciphertext_path).unwrap();
    let extra_descriptor = File::open("/dev/null").unwrap();
    let (report_read, report_write) = anonymous_pipe().unwrap();
    let (start_read, start_write) = anonymous_pipe().unwrap();
    let pid = unsafe { fork() };
    if pid < 0 {
        return false;
    }
    if pid == 0 {
        drop((report_read, start_write));
        let mut start = 0;
        if unsafe { read(start_read.as_raw_fd(), &mut start, 1) } != 1 {
            unsafe { _exit(34) }
        }
        drop(start_read);
        unsafe {
            run_guarded_retriever(|_| {
                let observed = test_support::exercise(tool, ciphertext, &audit, retirement);
                let values = [
                    observed.outcome.err().map_or(0, |f| f as u32),
                    u32::from(observed.destination_wiped),
                    u32::from(observed.tool_called),
                    u32::from(observed.tool_contract_checked),
                    u32::from(observed.tool_reaped),
                ];
                let report: Vec<_> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
                if write(report_write.as_raw_fd(), report.as_ptr(), report.len())
                    != report.len() as isize
                {
                    return Err(NativeFailure::IoUnavailable);
                }
                drop(extra_descriptor);
                if observed.tool_reaped {
                    Ok(())
                } else {
                    Err(NativeFailure::CleanupUnresolved)
                }
            })
        }
    }
    drop((report_write, start_read, tool, ciphertext, extra_descriptor));
    let identity = process_identity(pid as u32).unwrap();
    let mut exact = unsafe { ExactChild::inherit(identity) }.unwrap();
    assert_eq!(
        unsafe { write(start_write.as_raw_fd(), b"s".as_ptr(), 1) },
        1
    );
    drop(start_write);
    let waited = exact.reap(ExchangeDeadline::start(Duration::from_secs(2)).unwrap());
    let waited = if waited.is_err() {
        exact.reap(ExchangeDeadline::start(Duration::from_secs(2)).unwrap())
    } else {
        waited
    };
    let mut report = LockedMemory::<21>::allocate().unwrap();
    let read = PipeEndpoint::take(report_read, false).unwrap().read_frame(
        &mut report,
        20,
        ExchangeDeadline::start(Duration::from_secs(1)).unwrap(),
    );
    if waited != Ok(true) || read.is_err() {
        return false;
    }
    let observed: Vec<_> = report.bytes()[..20]
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let expected = if retirement != Retirement::Never {
        NativeFailure::GenerationMismatch as u32
    } else {
        match scenario {
            0 => 0,
            1 | 4 | 6 => NativeFailure::CredentialUnavailable as u32,
            2 | 7 => NativeFailure::CredentialPolicyRejected as u32,
            3 | 5 => NativeFailure::TimedOut as u32,
            _ => return false,
        }
    };
    let called = scenario != 7 && retirement != Retirement::BeforeTool;
    observed
        == [
            expected,
            u32::from(expected != 0),
            u32::from(called),
            u32::from(called),
            1,
        ]
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        std::process::exit(2);
    }
    let source = Path::new(&args[0]);
    let replacement = Path::new(&args[1]);
    let root = Path::new(&args[2]);
    let cases = [
        ("retained-tool-descriptor-exec", 0, Retirement::Never),
        ("retained-tool-short-output", 1, Retirement::Never),
        ("retained-tool-oversize-output", 2, Retirement::Never),
        ("retained-tool-requires-eof", 3, Retirement::Never),
        ("retained-tool-requires-zero-exit", 4, Retirement::Never),
        (
            "retained-tool-timeout-reaped-and-wiped",
            5,
            Retirement::Never,
        ),
        ("retained-tool-empty-output", 6, Retirement::Never),
        ("wrong-recipe-zero-tool-spawns", 7, Retirement::Never),
        (
            "retired-generation-zero-tool-spawns",
            0,
            Retirement::BeforeTool,
        ),
        ("retired-generation-zero-delivery", 0, Retirement::AfterTool),
    ];
    let mut failed = false;
    for (index, (name, scenario, retirement)) in cases.into_iter().enumerate() {
        let passed = case(
            &root.join(index.to_string()),
            source,
            replacement,
            scenario,
            retirement,
        );
        failed |= !passed;
        println!(
            "{{\"schema\":1,\"scope\":\"startup-exchange-composition\",\"case\":\"{name}\",\"result\":\"{}\"}}",
            if passed { "pass" } else { "red" }
        );
    }
    std::process::exit(i32::from(failed));
}
