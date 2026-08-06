use std::path::{Path, PathBuf};

use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType,
    PrimaryDeviceAttributes, SecondaryDeviceAttributes, TertiaryDeviceAttributes,
};

pub(crate) const TERM_FALLBACK: &str = "xterm-256color";
pub(crate) const TERM_NAME: &str = "xterm-spaceterm";
pub(crate) const PROGRAM_NAME: &str = "SpaceTerm";
pub(crate) const PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const XTVERSION: &str = concat!("SpaceTerm ", env!("CARGO_PKG_VERSION"));
pub(crate) const COLORTERM: &str = "truecolor";

const MAX_XTGETTCAP_REQUEST: usize = 2_048;
const MAX_XTGETTCAP_NAMES: usize = 16;
const MAX_XTGETTCAP_NAME: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchIdentity {
    pub(crate) term: &'static str,
    pub(crate) terminfo: Option<PathBuf>,
}

pub(crate) fn launch_identity(resource_root: &Path) -> LaunchIdentity {
    let terminfo = resource_root.join("terminfo");
    let decimal_entry = terminfo.join("78").join(TERM_NAME);
    let character_entry = terminfo.join("x").join(TERM_NAME);
    if decimal_entry.is_file() || character_entry.is_file() {
        LaunchIdentity {
            term: TERM_NAME,
            terminfo: Some(terminfo),
        }
    } else {
        LaunchIdentity {
            term: TERM_FALLBACK,
            terminfo: None,
        }
    }
}

pub(crate) const fn device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(
            ConformanceLevel::VT220,
            &[
                DeviceAttributeFeature::ANSI_COLOR,
                DeviceAttributeFeature::CLIPBOARD,
            ],
        ),
        secondary: SecondaryDeviceAttributes {
            device_type: DeviceType::VT220,
            firmware_version: 0,
            rom_cartridge: 0,
        },
        tertiary: TertiaryDeviceAttributes { unit_id: 0 },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapabilityValue {
    Boolean,
    Value(&'static [u8]),
}

fn capability(name: &[u8], terminal_name: &'static str) -> Option<CapabilityValue> {
    match name {
        b"TN" => Some(CapabilityValue::Value(terminal_name.as_bytes())),
        b"Co" => Some(CapabilityValue::Value(b"256")),
        b"RGB" => Some(CapabilityValue::Value(b"8")),
        b"Tc" => Some(CapabilityValue::Boolean),
        b"Ms" => Some(CapabilityValue::Value(b"\x1b]52;%p1%s;%p2%s\x07")),
        b"Ss" => Some(CapabilityValue::Value(b"\x1b[%p1%d q")),
        b"Se" => Some(CapabilityValue::Value(b"\x1b[2 q")),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) struct XtGetTcapObserver {
    state: ObserverState,
    terminal_name: &'static str,
}

#[derive(Debug, Default)]
enum ObserverState {
    #[default]
    Ground,
    Escape,
    Dcs {
        bytes: Vec<u8>,
        escaped: bool,
        overflowed: bool,
    },
}

impl XtGetTcapObserver {
    pub(crate) fn new(terminal_name: &'static str) -> Self {
        Self {
            state: ObserverState::Ground,
            terminal_name,
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8], replies: &mut Vec<u8>) {
        for &byte in bytes {
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                ObserverState::Ground => match byte {
                    0x1b => ObserverState::Escape,
                    0x90 => ObserverState::Dcs {
                        bytes: Vec::new(),
                        escaped: false,
                        overflowed: false,
                    },
                    _ => ObserverState::Ground,
                },
                ObserverState::Escape => {
                    if byte == b'P' {
                        ObserverState::Dcs {
                            bytes: Vec::new(),
                            escaped: false,
                            overflowed: false,
                        }
                    } else if byte == 0x1b {
                        ObserverState::Escape
                    } else {
                        ObserverState::Ground
                    }
                }
                ObserverState::Dcs {
                    mut bytes,
                    mut escaped,
                    mut overflowed,
                } => {
                    if byte == 0x9c || (escaped && byte == b'\\') {
                        if !overflowed {
                            append_xtgettcap_replies(&bytes, self.terminal_name, replies);
                        }
                        ObserverState::Ground
                    } else {
                        if escaped {
                            if !overflowed {
                                if bytes.len() < MAX_XTGETTCAP_REQUEST {
                                    bytes.push(0x1b);
                                } else {
                                    overflowed = true;
                                }
                            }
                            escaped = false;
                        }
                        if byte == 0x1b {
                            escaped = true;
                        } else if !overflowed {
                            if bytes.len() < MAX_XTGETTCAP_REQUEST {
                                bytes.push(byte);
                            } else {
                                overflowed = true;
                            }
                        }
                        ObserverState::Dcs {
                            bytes,
                            escaped,
                            overflowed,
                        }
                    }
                }
            };
        }
    }
}

fn append_xtgettcap_replies(request: &[u8], terminal_name: &'static str, replies: &mut Vec<u8>) {
    let Some(names) = request.strip_prefix(b"+q") else {
        return;
    };
    for encoded_name in names.split(|byte| *byte == b';').take(MAX_XTGETTCAP_NAMES) {
        let Some(name) = decode_hex_name(encoded_name) else {
            continue;
        };
        replies.extend_from_slice(b"\x1bP");
        if let Some(value) = capability(&name, terminal_name) {
            replies.extend_from_slice(b"1+r");
            replies.extend_from_slice(encoded_name);
            if let CapabilityValue::Value(value) = value {
                replies.push(b'=');
                append_hex(value, replies);
            }
        } else {
            replies.extend_from_slice(b"0+r");
            replies.extend_from_slice(encoded_name);
        }
        replies.extend_from_slice(b"\x1b\\");
    }
}

fn decode_hex_name(encoded: &[u8]) -> Option<Vec<u8>> {
    if encoded.is_empty()
        || !encoded.len().is_multiple_of(2)
        || encoded.len() > MAX_XTGETTCAP_NAME * 2
    {
        return None;
    }
    encoded
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(pair[0])?;
            let low = hex_value(pair[1])?;
            let value = (high << 4) | low;
            value.is_ascii_graphic().then_some(value)
        })
        .collect()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn append_hex(value: &[u8], output: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value {
        output.push(HEX[usize::from(byte >> 4)]);
        output.push(HEX[usize::from(byte & 0x0f)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_resources() -> PathBuf {
        std::env::temp_dir().join(format!(
            "spaceterm-terminfo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn packaged_terminfo_selects_spaceterm_identity_and_missing_resources_fall_back() {
        let resources = temporary_resources();
        let terminfo = resources.join("terminfo/78/xterm-spaceterm");
        std::fs::create_dir_all(terminfo.parent().unwrap()).unwrap();
        std::fs::write(&terminfo, b"compiled").unwrap();

        assert_eq!(
            launch_identity(&resources),
            LaunchIdentity {
                term: TERM_NAME,
                terminfo: Some(resources.join("terminfo")),
            }
        );
        assert_eq!(
            launch_identity(Path::new("/definitely/missing")),
            LaunchIdentity {
                term: TERM_FALLBACK,
                terminfo: None,
            }
        );
        std::fs::remove_dir_all(resources).unwrap();
    }

    #[test]
    fn xtgettcap_answers_only_bounded_truthful_capabilities_across_chunks() {
        let mut observer = XtGetTcapObserver::new(TERM_NAME);
        let mut replies = Vec::new();
        observer.feed(b"\x1bP+q544E;436F;5463;626F677573\x1b", &mut replies);
        observer.feed(b"\\", &mut replies);

        assert_eq!(
            replies,
            b"\x1bP1+r544E=787465726D2D73706163657465726D\x1b\\\
              \x1bP1+r436F=323536\x1b\\\
              \x1bP1+r5463\x1b\\\
              \x1bP0+r626F677573\x1b\\"
        );

        replies.clear();
        let oversized = vec![b'A'; MAX_XTGETTCAP_REQUEST + 1];
        observer.feed(b"\x1bP+q", &mut replies);
        observer.feed(&oversized, &mut replies);
        observer.feed(b"\x1b\\", &mut replies);
        assert!(replies.is_empty());

        let mut fallback = XtGetTcapObserver::new(TERM_FALLBACK);
        fallback.feed(b"\x1bP+q544E\x1b\\", &mut replies);
        assert_eq!(replies, b"\x1bP1+r544E=787465726D2D323536636F6C6F72\x1b\\");
    }

    #[test]
    fn terminfo_source_and_xtgettcap_share_the_verified_capability_profile() {
        let source = include_str!("../../assets/terminfo/xterm-spaceterm.terminfo");
        for capability in ["colors#256", "RGB#8", "Tc", "Ms=", "Ss=", "Se="] {
            assert!(source.contains(capability), "missing {capability}");
        }
        assert!(source.contains("use=xterm-256color"));
        assert!(!source.to_ascii_lowercase().contains("ghostty"));
        assert!(!XTVERSION.to_ascii_lowercase().contains("ghostty"));
    }
}
