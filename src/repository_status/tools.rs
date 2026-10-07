//! Tool version parsing and readiness.

use super::{GitToolStatus, MINIMUM_GIT_VERSION, ToolVersion};

/// The oldest git whose built-in fsmonitor daemon Repository Status uses.
pub(crate) const BUILTIN_FSMONITOR_GIT_VERSION: ToolVersion = ToolVersion {
    major: 2,
    minor: 36,
    patch: 0,
};

/// Parses `git --version` output, such as `git version 2.39.5 (Apple Git-154)`.
pub(crate) fn parse_git_version(output: &[u8]) -> Option<ToolVersion> {
    parse_version(output, "git version ")
}

pub(crate) fn git_tool_status(version: ToolVersion) -> GitToolStatus {
    if version >= MINIMUM_GIT_VERSION {
        GitToolStatus::Ready(version)
    } else {
        GitToolStatus::TooOld(version)
    }
}

/// Reads `major.minor[.patch]` after `prefix` on the first line. Vendor suffixes such as
/// `.windows.1` or `-rc1` after the numbers are ignored.
fn parse_version(output: &[u8], prefix: &str) -> Option<ToolVersion> {
    let first_line = output.split(|byte| *byte == b'\n').next()?;
    let line = std::str::from_utf8(first_line).ok()?.trim_end_matches('\r');
    let token = line.strip_prefix(prefix)?.split(' ').next()?;
    let mut components = token.split('.');
    let major = leading_number(components.next()?)?;
    let minor = leading_number(components.next()?)?;
    let patch = match components.next() {
        Some(component) => leading_number(component)?,
        None => 0,
    };
    Some(ToolVersion {
        major,
        minor,
        patch,
    })
}

fn leading_number(component: &str) -> Option<u16> {
    let end = component
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(component.len());
    component[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn version(major: u16, minor: u16, patch: u16) -> ToolVersion {
        ToolVersion {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn git_versions_should_parse_with_vendor_suffixes() {
        for (output, expected) in [
            ("git version 2.39.5 (Apple Git-154)\n", version(2, 39, 5)),
            ("git version 2.47.0\n", version(2, 47, 0)),
            ("git version 2.43.0.windows.1\n", version(2, 43, 0)),
            ("git version 2.45.0-rc1\n", version(2, 45, 0)),
            ("git version 2.40\n", version(2, 40, 0)),
            ("git version 2.54.0 (Apple Git-157)\r\n", version(2, 54, 0)),
            ("git version 3.0.0", version(3, 0, 0)),
        ] {
            assert_eq!(
                parse_git_version(output.as_bytes()),
                Some(expected),
                "{output:?}"
            );
        }
    }

    #[test]
    fn unrecognized_version_output_should_be_rejected() {
        for output in [
            &b""[..],
            b"git version \n",
            b"git version x.y.z\n",
            b"git version 2\n",
            b"git version .39.1\n",
            b"git version 2..1\n",
            b"git version 70000.0.0\n",
            b"Git version 2.39.5\n",
            b"gh version 2.62.0\n",
            b"\ngit version 2.39.5\n",
            b"git version \xff2.39.5\n",
        ] {
            assert_eq!(parse_git_version(output), None, "{output:?}");
        }
    }

    #[test]
    fn git_status_should_require_the_minimum_version() {
        assert_eq!(
            git_tool_status(MINIMUM_GIT_VERSION),
            GitToolStatus::Ready(MINIMUM_GIT_VERSION)
        );
        assert_eq!(
            git_tool_status(version(2, 39, 5)),
            GitToolStatus::Ready(version(2, 39, 5))
        );
        assert_eq!(
            git_tool_status(version(2, 14, 9)),
            GitToolStatus::TooOld(version(2, 14, 9))
        );
        assert_eq!(
            git_tool_status(version(1, 99, 0)),
            GitToolStatus::TooOld(version(1, 99, 0))
        );
    }
}
