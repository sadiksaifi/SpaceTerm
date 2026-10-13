use spaceterm_core::bootstrap::*;
use spaceterm_host::bootstrap::process::BootstrapProbeFailure;
use spaceterm_host::bootstrap::{
    command::MAX_PROBE_BYTES, probe::ReadOnlyHostProbe, process::read_bounded_probe,
};

const LINUX: &str = "STBP2\nuid=1000\nos=Linux\narch=x86_64\nversion=0.0.0\nlibc=glibc 2.35\nkernel=5.15.0-91-generic\nmanager=systemd-user\nmanager_version=249\nmanager_reachable=yes\nlinger=yes\nsudo=available\n";
#[test]
fn probe_reads_user_manager_and_enabled_linger_without_credentials() {
    let probe = ReadOnlyHostProbe::parse(LINUX.as_bytes(), 1000).unwrap();
    assert_eq!(
        probe.supervision,
        BootstrapSupervision::SystemdUser {
            version: 249,
            reachable: true
        }
    );
    assert_eq!(probe.linger, BootstrapLinger::Enabled);
    assert_eq!(probe.sudo, BootstrapSudoAvailability::Available);
    assert_eq!(probe.libc, BootstrapLibc::Glibc(HostVersion::new(2, 35, 0)));
}
#[test]
fn probe_distinguishes_missing_gui_domain_from_available_launchctl() {
    let text = "STBP2\nuid=1000\nos=Darwin\narch=arm64\nversion=26.0\nlibc=none\nkernel=25.0.0\nmanager=launchagent\nmanager_version=1\nmanager_reachable=no\nlinger=none\nsudo=unavailable\n";
    let probe = ReadOnlyHostProbe::parse(text.as_bytes(), 1000).unwrap();
    assert_eq!(probe.os_version, HostVersion::new(26, 0, 0));
    assert_eq!(
        probe.supervision,
        BootstrapSupervision::LaunchAgent { gui_domain: false }
    );
    assert_eq!(probe.linger, BootstrapLinger::NotApplicable);
}
#[test]
fn probe_rejects_malicious_bytes_metacharacters_and_duplicate_fields() {
    for (from, to) in [
        ("x86_64", "x86_64;id"),
        ("Linux", "$(id)"),
        ("2.35", "2.35\0"),
        ("249", "249\u{1b}[0m"),
        ("uid=1000", "uid=1000\nuid=1000"),
        ("manager=systemd-user", "manager=launchagent"),
        ("kernel=5.15.0-91-generic", "kernel=5.15.0-`id`"),
        ("2.35", "02.35"),
        ("uid=1000", "uid=1000\r"),
        ("linger=yes", "linger=none"),
        ("manager_reachable=yes", "manager_reachable=maybe"),
    ] {
        let bytes = LINUX.replace(from, to);
        assert!(
            ReadOnlyHostProbe::parse(bytes.as_bytes(), 1000).is_err(),
            "accepted replacement {from}"
        );
    }
    let mut bytes = LINUX.as_bytes().to_vec();
    bytes.push(0xff);
    assert!(ReadOnlyHostProbe::parse(&bytes, 1000).is_err());
    assert!(ReadOnlyHostProbe::parse(format!("{LINUX}trailing\n").as_bytes(), 1000).is_err());
}
#[test]
fn probe_rejects_root_and_changed_account_bindings() {
    for uid in ["0", "1001", "4294967295"] {
        let result = ReadOnlyHostProbe::parse(
            LINUX.replace("uid=1000", &format!("uid={uid}")).as_bytes(),
            1000,
        );
        assert_eq!(result.err().unwrap(), BootstrapProbeFailure::RouteChanged);
    }
}
#[test]
fn probe_rejects_oversized_output_before_parsing_or_unbounded_read() {
    let bytes = vec![b'x'; MAX_PROBE_BYTES + 1];
    assert_eq!(
        ReadOnlyHostProbe::parse(&bytes, 1000).err().unwrap(),
        BootstrapProbeFailure::LimitExceeded
    );
    assert_eq!(
        read_bounded_probe(std::io::repeat(b'x')).err().unwrap(),
        BootstrapProbeFailure::LimitExceeded
    );
}
#[test]
fn probe_accepts_debian_kernel_build_suffix_and_unavailable_linger() {
    let parsed = ReadOnlyHostProbe::parse(
        LINUX
            .replace("5.15.0-91-generic", "6.12.43+deb13-amd64")
            .replace("linger=yes", "linger=unavailable")
            .as_bytes(),
        1000,
    )
    .unwrap();
    assert_eq!(parsed.kernel, HostVersion::new(6, 12, 43));
    assert_eq!(parsed.linger, BootstrapLinger::Unavailable);
}

#[test]
fn probe_accepts_ubuntu_zero_padded_release_components() {
    let parsed = ReadOnlyHostProbe::parse(
        LINUX.replace("version=0.0.0", "version=22.04").as_bytes(),
        1000,
    )
    .unwrap();
    assert_eq!(parsed.os_version, HostVersion::new(22, 4, 0));
}
