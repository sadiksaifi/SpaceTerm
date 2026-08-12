use std::env;
use std::fs::Metadata;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const SOCKET_ENV: &str = "SPACETERM_ACCEPTANCE_SOCKET";
const CHALLENGE_SCHEMA: &str = "spaceterm.acceptance.native-launch-challenge/v1";
const OBSERVATION_SCHEMA: &str = "spaceterm.acceptance.native-launch-proof/v2";
const MAX_FRAME_BYTES: usize = 16 * 1024;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

static REQUEST: OnceLock<Mutex<Option<ObservationRequest>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ObservationGeometry {
    pub(crate) rows: u16,
    pub(crate) columns: u16,
    pub(crate) logical_width: f32,
    pub(crate) logical_height: f32,
    pub(crate) backing_pixel_width: u32,
    pub(crate) backing_pixel_height: u32,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AcceptanceObservationError {
    #[error("acceptance observation socket is not an absolute private Unix socket")]
    InvalidSocket,
    #[error("acceptance observation challenge is invalid")]
    InvalidChallenge,
    #[error("acceptance observation was configured more than once")]
    ConfiguredTwice,
    #[error("acceptance observation failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug)]
struct ObservationRequest {
    stream: UnixStream,
    nonce: String,
    run_id: String,
    app_sha256: String,
    initial: Option<ObservationSelection>,
}

#[derive(Debug)]
pub(crate) struct PreparedObservation {
    request: ObservationRequest,
    initial: ObservationSelection,
}

#[derive(Debug)]
struct ObservationSelection {
    selected_font: String,
    geometry: ObservationGeometry,
}

pub(crate) fn configure_from_environment() -> Result<(), AcceptanceObservationError> {
    let socket_path = take_socket_environment()?;
    let Some(socket_path) = socket_path else {
        return Ok(());
    };
    let mut stream = connect_private_socket(&socket_path)?;
    let challenge = read_frame(&mut stream)?;
    let (nonce, run_id, app_sha256) = parse_challenge(&challenge)?;
    REQUEST
        .set(Mutex::new(Some(ObservationRequest {
            stream,
            nonce,
            run_id,
            app_sha256,
            initial: None,
        })))
        .map_err(|_| AcceptanceObservationError::ConfiguredTwice)
}

pub(crate) fn claim_session(selected_font: &str, geometry: ObservationGeometry) -> bool {
    let Some(request) = REQUEST.get() else {
        return false;
    };
    let mut request = request
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(request) = request.as_mut() else {
        return false;
    };
    if request.initial.is_some() {
        return false;
    }
    request.initial = Some(ObservationSelection {
        selected_font: selected_font.to_owned(),
        geometry,
    });
    true
}

pub(crate) fn update_geometry(geometry: ObservationGeometry) {
    let Some(request) = REQUEST.get() else {
        return;
    };
    let mut request = request
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(initial) = request
        .as_mut()
        .and_then(|request| request.initial.as_mut())
    else {
        return;
    };
    initial.geometry = geometry;
}

pub(crate) fn prepare_once(rows: u16, columns: u16) -> Option<PreparedObservation> {
    let request = REQUEST.get()?;
    let mut request = request
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let initial = request.as_ref()?.initial.as_ref()?;
    if initial.geometry.rows != rows || initial.geometry.columns != columns {
        return None;
    }
    let mut request = request.take()?;
    let initial = request.initial.take()?;
    Some(PreparedObservation { request, initial })
}

impl PreparedObservation {
    pub(crate) fn emit(mut self) -> Result<(), AcceptanceObservationError> {
        let executable = env::current_exe()?.canonicalize()?;
        let executable_metadata = executable.metadata()?;
        let record = format_observation(
            &self.request,
            &self.initial.selected_font,
            self.initial.geometry,
            &executable,
            executable_metadata.dev(),
            executable_metadata.ino(),
        );
        write_frame(&mut self.request.stream, record.as_bytes())?;
        self.request.stream.shutdown(std::net::Shutdown::Both)?;
        Ok(())
    }
}

fn take_socket_environment() -> Result<Option<PathBuf>, AcceptanceObservationError> {
    let value = env::var_os(SOCKET_ENV);
    // This runs at the beginning of main, before GPUI creates worker threads. The socket name is
    // never inherited by the Shell Process, and the connected descriptor is close-on-exec.
    unsafe {
        env::remove_var(SOCKET_ENV);
    }
    value
        .map(|value| {
            value
                .into_string()
                .map(PathBuf::from)
                .map_err(|_| AcceptanceObservationError::InvalidSocket)
        })
        .transpose()
}

fn connect_private_socket(path: &Path) -> Result<UnixStream, AcceptanceObservationError> {
    if !path.is_absolute() {
        return Err(AcceptanceObservationError::InvalidSocket);
    }
    let Some(parent) = path.parent() else {
        return Err(AcceptanceObservationError::InvalidSocket);
    };
    let parent_metadata = parent.symlink_metadata()?;
    let socket_metadata = path.symlink_metadata()?;
    if parent_metadata.file_type().is_symlink()
        || !parent_metadata.file_type().is_dir()
        || !is_private_owner(&parent_metadata)
        || !socket_metadata.file_type().is_socket()
        || !is_private_owner(&socket_metadata)
    {
        return Err(AcceptanceObservationError::InvalidSocket);
    }
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    set_close_on_exec(&stream)?;
    Ok(stream)
}

fn set_close_on_exec(stream: &UnixStream) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn is_private_owner(metadata: &Metadata) -> bool {
    metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0
}

fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, AcceptanceObservationError> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if !(1..=MAX_FRAME_BYTES).contains(&length) {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    let mut frame = vec![0_u8; length];
    stream.read_exact(&mut frame)?;
    Ok(frame)
}

fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| (1..=MAX_FRAME_BYTES as u32).contains(length))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid frame length"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

fn parse_challenge(
    challenge: &[u8],
) -> Result<(String, String, String), AcceptanceObservationError> {
    let challenge =
        std::str::from_utf8(challenge).map_err(|_| AcceptanceObservationError::InvalidChallenge)?;
    let records = parse_records(challenge)?;
    let [schema, nonce, run_id, app_sha256] = records.as_slice() else {
        return Err(AcceptanceObservationError::InvalidChallenge);
    };
    if schema != &("schema", CHALLENGE_SCHEMA.to_owned())
        || nonce.0 != "launch.nonce"
        || !is_lower_hex(&nonce.1, 64)
        || run_id.0 != "run.id"
        || !is_run_id(&run_id.1)
        || app_sha256.0 != "package.app.sha256"
        || !is_lower_hex(&app_sha256.1, 64)
    {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    Ok((nonce.1.clone(), run_id.1.clone(), app_sha256.1.clone()))
}

fn parse_records(value: &str) -> Result<Vec<(&str, String)>, AcceptanceObservationError> {
    if !value.ends_with('\n') {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    value
        .strip_suffix('\n')
        .expect("trailing newline was checked")
        .split('\n')
        .map(|line| {
            let (key, value) = line
                .split_once('\t')
                .ok_or(AcceptanceObservationError::InvalidChallenge)?;
            if key.is_empty() || value.contains('\t') {
                return Err(AcceptanceObservationError::InvalidChallenge);
            }
            Ok((key, decode_value(value)?))
        })
        .collect()
}

fn format_observation(
    request: &ObservationRequest,
    selected_font: &str,
    geometry: ObservationGeometry,
    executable: &Path,
    executable_device: u64,
    executable_inode: u64,
) -> String {
    let mut output = String::new();
    for (key, value) in [
        ("schema", OBSERVATION_SCHEMA.to_owned()),
        ("observation.source", "production-app".to_owned()),
        ("launch.nonce", request.nonce.clone()),
        ("run.id", request.run_id.clone()),
        ("package.app.sha256", request.app_sha256.clone()),
        ("process.pid", std::process::id().to_string()),
        (
            "process.executable.path",
            executable.to_string_lossy().into_owned(),
        ),
        ("process.executable.device", executable_device.to_string()),
        ("process.executable.inode", executable_inode.to_string()),
        ("terminal_font_selected", selected_font.to_owned()),
        ("initial_grid.rows", geometry.rows.to_string()),
        ("initial_grid.columns", geometry.columns.to_string()),
        (
            "initial_grid.logical_width",
            decimal(geometry.logical_width),
        ),
        (
            "initial_grid.logical_height",
            decimal(geometry.logical_height),
        ),
        (
            "initial_grid.backing_pixel_width",
            geometry.backing_pixel_width.to_string(),
        ),
        (
            "initial_grid.backing_pixel_height",
            geometry.backing_pixel_height.to_string(),
        ),
        ("observation.complete", "true".to_owned()),
    ] {
        output.push_str(key);
        output.push('\t');
        output.push_str(&encode_value(&value));
        output.push('\n');
    }
    output
}

fn encode_value(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\t', "%09")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn decode_value(value: &str) -> Result<String, AcceptanceObservationError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(AcceptanceObservationError::InvalidChallenge);
        }
        let byte = match &bytes[index + 1..=index + 2] {
            b"25" => b'%',
            b"09" => b'\t',
            b"0D" => b'\r',
            b"0A" => b'\n',
            _ => return Err(AcceptanceObservationError::InvalidChallenge),
        };
        decoded.push(byte);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| AcceptanceObservationError::InvalidChallenge)
}

fn decimal(value: f32) -> String {
    let formatted = format!("{value:.6}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_run_id(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => true,
            b'.' | b'_' | b'-' => index > 0,
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(stream: UnixStream) -> ObservationRequest {
        ObservationRequest {
            stream,
            nonce: "a".repeat(64),
            run_id: "i43-proof".to_owned(),
            app_sha256: "b".repeat(64),
            initial: None,
        }
    }

    #[test]
    fn challenge_should_be_exact_and_bounded() {
        let challenge = format!(
            "schema\t{CHALLENGE_SCHEMA}\nlaunch.nonce\t{}\nrun.id\ti43-proof\npackage.app.sha256\t{}\n",
            "a".repeat(64),
            "b".repeat(64),
        );
        let (_, run_id, _) = parse_challenge(challenge.as_bytes()).unwrap();
        assert_eq!(run_id, "i43-proof");

        assert!(parse_challenge(format!("{challenge}extra\ttrue\n").as_bytes()).is_err());
        assert!(parse_challenge(challenge.trim_end().as_bytes()).is_err());
    }

    #[test]
    fn observation_should_bind_runtime_facts_without_terminal_content() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let record = format_observation(
            &request(stream),
            "JetBrainsMono Nerd Font",
            ObservationGeometry {
                rows: 24,
                columns: 80,
                logical_width: 800.5,
                logical_height: 480.0,
                backing_pixel_width: 1601,
                backing_pixel_height: 960,
            },
            Path::new("/Volumes/SpaceTerm/SpaceTerm.app/Contents/MacOS/SpaceTerm"),
            7,
            11,
        );

        assert!(record.contains("observation.source\tproduction-app\n"));
        assert!(record.contains("initial_grid.logical_width\t800.5\n"));
        assert!(record.contains("process.executable.inode\t11\n"));
        assert!(record.contains("observation.complete\ttrue\n"));
        assert!(!record.contains("terminal.content"));
    }

    #[test]
    fn value_encoding_should_reject_noncanonical_escapes() {
        assert_eq!(decode_value("100%25%09ok").unwrap(), "100%\tok");
        assert!(decode_value("%2f").is_err());
        assert!(decode_value("%").is_err());
    }
}
