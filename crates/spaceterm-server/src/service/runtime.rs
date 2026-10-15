//! Authenticate retained artifacts before selecting or executing their runtime.
use super::*;
use base64::Engine;
use std::{collections::BTreeSet, io::Read, os::unix::fs::MetadataExt};

const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;

pub(super) fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}

pub(super) fn verify(path: &Path, identity: &str) -> Result<(), ServerError> {
    let directory = owned_directory(path)?;
    let bytes = read_file(&directory, Path::new("artifact.tar.gz"), MAX_ARCHIVE)?;
    let signature = read_file(&directory, Path::new("artifact.sig"), 64)?;
    let hash = ring::digest::digest(&ring::digest::SHA256, &bytes);
    let digest: String = hash.as_ref().iter().map(|v| format!("{v:02x}")).collect();
    if path.file_name().and_then(|v| v.to_str()) != Some(&digest) {
        return Err(ServerError::RuntimeUnavailable);
    }
    let template = include_str!("../../../../packaging/macos/spaceterm/Info.plist");
    let key = template
        .split_once("<key>SUPublicEDKey</key>")
        .and_then(|(_, tail)| tail.trim_start().strip_prefix("<string>"))
        .and_then(|tail| tail.split_once("</string>"))
        .ok_or(ServerError::RuntimeUnavailable)?
        .0;
    let key = base64::engine::general_purpose::STANDARD
        .decode(key.trim())
        .map_err(|_| ServerError::RuntimeUnavailable)?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(&bytes, &signature)
        .map_err(|_| ServerError::RuntimeUnavailable)?;
    verify_contents(&directory, &bytes, identity)?;
    #[cfg(target_os = "macos")]
    {
        let template = include_str!("../../../../packaging/macos/spaceterm/Packager.toml");
        let signer = template
            .lines()
            .find_map(|line| line.strip_prefix("signing-identity = \""))
            .and_then(|v| v.strip_suffix('"'))
            .ok_or(ServerError::RuntimeUnavailable)?;
        let requirement = format!(
            "identifier \"io.github.sadiksaifi.{identity}.server\" and certificate leaf = H\"{signer}\""
        );
        command(
            "/usr/bin/codesign",
            &[
                "--verify".into(),
                "--strict".into(),
                "-R".into(),
                format!("={requirement}").into(),
                path.join("bin/spaceterm-server").into_os_string(),
            ],
        )
        .map_err(|_| ServerError::RuntimeUnavailable)?;
    }
    verify_directory_path(&directory, path)
}

pub(super) fn describe(path: &Path, identity: &str) -> Result<(String, String), ServerError> {
    verify(path, identity)?;
    let directory = owned_directory(path)?;
    let bytes = read_file(&directory, Path::new("build.json"), 4096)?;
    let info: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| ServerError::RuntimeUnavailable)?;
    let version = info["version"]
        .as_str()
        .ok_or(ServerError::RuntimeUnavailable)?;
    let commit = info["commit"]
        .as_str()
        .ok_or(ServerError::RuntimeUnavailable)?;
    if version.len() > 64 || commit.len() != 40 || !commit.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err(ServerError::RuntimeUnavailable);
    }
    Ok((version.to_owned(), commit.to_owned()))
}

fn verify_contents(directory: &File, bytes: &[u8], identity: &str) -> Result<(), ServerError> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut names = BTreeSet::new();
    let mut expanded = 0u64;
    let mut manifest = false;
    let mut executable = false;
    for item in archive
        .entries()
        .map_err(|_| ServerError::RuntimeUnavailable)?
    {
        let item = item.map_err(|_| ServerError::RuntimeUnavailable)?;
        let path = item
            .path()
            .map_err(|_| ServerError::RuntimeUnavailable)?
            .into_owned();
        let path = path
            .strip_prefix("server")
            .map_err(|_| ServerError::RuntimeUnavailable)?;
        if path
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
            || names.len() >= 128
            || !names.insert(path.to_owned())
        {
            return Err(ServerError::RuntimeUnavailable);
        }
        let size = item.size();
        expanded = expanded
            .checked_add(size)
            .ok_or(ServerError::RuntimeUnavailable)?;
        if expanded > MAX_EXPANDED {
            return Err(ServerError::RuntimeUnavailable);
        }
        if item.header().entry_type().is_dir() {
            continue;
        }
        if !item.header().entry_type().is_file() {
            return Err(ServerError::RuntimeUnavailable);
        }
        let retained = read_file(directory, path, size)?;
        let mut signed = Vec::new();
        item.take(size + 1)
            .read_to_end(&mut signed)
            .map_err(|_| ServerError::RuntimeUnavailable)?;
        if signed != retained {
            return Err(ServerError::RuntimeUnavailable);
        }
        if path == Path::new("build.json") {
            let info: serde_json::Value =
                serde_json::from_slice(&signed).map_err(|_| ServerError::RuntimeUnavailable)?;
            #[cfg(target_os = "macos")]
            let target = "aarch64-apple-darwin";
            #[cfg(target_os = "linux")]
            let target = "x86_64-unknown-linux-gnu";
            if info["schema"] != 1 || info["identity"] != identity || info["target"] != target {
                return Err(ServerError::RuntimeUnavailable);
            }
            manifest = true;
        }
        if path == Path::new("bin/spaceterm-server") {
            executable = true;
        }
    }
    if !manifest || !executable {
        return Err(ServerError::RuntimeUnavailable);
    }
    Ok(())
}

fn read_file(directory: &File, path: &Path, maximum: u64) -> Result<Vec<u8>, ServerError> {
    let file = open_relative(directory, path, false)?;
    let metadata = file
        .metadata()
        .map_err(|_| ServerError::StorageUnavailable)?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > maximum {
        return Err(ServerError::RuntimeUnavailable);
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ServerError::StorageUnavailable)?;
    if bytes.len() as u64 > maximum {
        return Err(ServerError::RuntimeUnavailable);
    }
    Ok(bytes)
}
