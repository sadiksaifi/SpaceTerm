//! Native Adapter integration evidence.
use crate::platform::launch_host::resource_root;
use std::process::Command;
#[test]
fn every_supported_resource_uses_the_same_version_and_protocol_marks() {
    let root = resource_root().join("shell-integration");
    assert_eq!(
        std::fs::read_to_string(root.join("VERSION"))
            .unwrap()
            .trim(),
        "1"
    );
    for relative in [
        "bash/spaceterm.bash",
        "elvish/lib/spaceterm-integration.elv",
        "fish/vendor_conf.d/spaceterm-shell-integration.fish",
        "nushell/vendor/autoload/spaceterm.nu",
        "zsh/spaceterm-integration",
    ] {
        let script = std::fs::read_to_string(root.join(relative)).unwrap();
        assert!(
            script.contains("133;"),
            "{relative} must emit OSC 133 marks"
        );
        assert!(
            script.contains("SPACETERM_SHELL_INTEGRATION_VERSION"),
            "{relative} must verify the resource handshake"
        );
    }
}

#[test]
fn zsh_prompt_hook_renders_protocol_marker_once_without_changing_printable_prompt() {
    let integration = resource_root().join("shell-integration/zsh/spaceterm-integration");
    let output = Command::new("/bin/zsh")
        .args([
            "-dfi",
            "-c",
            r#"PS1='SPACE> '; builtin source -- "$1"; _spaceterm_command_active=1; (builtin exit 7); "$precmd_functions[-1]"; "$precmd_functions[-1]"; builtin print -nrP -- "$PS1""#,
            "spaceterm",
        ])
        .arg(integration)
        .env("SPACETERM_SHELL_INTEGRATION_VERSION", "1")
        .output()
        .unwrap();
    let completion = b"\x1b]133;D;7\x07";
    let prompt_marker = b"\x1b]133;B\x07";
    let literal_prompt_marker = br"\e]133;B\a";
    let reported_prior_status = output
        .stdout
        .windows(completion.len())
        .any(|window| window == completion);
    let rendered_prompt_markers = output
        .stdout
        .windows(prompt_marker.len())
        .filter(|window| *window == prompt_marker)
        .count();
    let rendered_literal_marker = output
        .stdout
        .windows(literal_prompt_marker.len())
        .any(|window| window == literal_prompt_marker);
    let rendered_prompt_is_preserved = output.stdout.ends_with(b"SPACE> \x1b]133;B\x07");

    assert_eq!(
        (
            output.status.success(),
            output.stderr.as_slice(),
            reported_prior_status,
            rendered_prompt_markers,
            rendered_literal_marker,
            rendered_prompt_is_preserved,
        ),
        (true, &[][..], true, 1, false, true),
        "stdout: {:?}",
        String::from_utf8_lossy(&output.stdout),
    );
}
