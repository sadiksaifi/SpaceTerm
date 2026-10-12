use crate::DistributionFailure;
use serde::{Deserialize, Serialize};

// Keep this recognition identical to the shipped desktop updater, including u64 bounds.
pub(crate) fn stable_version(value: &str) -> Option<[u64; 3]> {
    if value.len() > 48 {
        return None;
    }
    let mut parts = value.split('.');
    let mut result = [0; 3];
    for number in &mut result {
        let part = parts.next()?;
        if part.is_empty()
            || part.len() > 1 && part.starts_with('0')
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some(result)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StableVersion(String);

impl StableVersion {
    pub fn parse(value: &str) -> Result<Self, DistributionFailure> {
        stable_version(value).ok_or(DistributionFailure::InvalidRecord)?;
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn supersedes(&self, current: &str) -> bool {
        match (stable_version(&self.0), stable_version(current)) {
            (Some(candidate), Some(current)) => candidate > current,
            _ => false,
        }
    }
}
impl TryFrom<String> for StableVersion {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).map_err(|_| "invalid version")
    }
}
impl From<StableVersion> for String {
    fn from(value: StableVersion) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BuildVersion(String);
impl BuildVersion {
    pub fn parse(value: &str) -> Result<Self, DistributionFailure> {
        let development = value
            .strip_prefix("dev.")
            .map(|v| v.strip_suffix(".dirty").unwrap_or(v));
        if stable_version(value).is_none()
            && !development.is_some_and(|v| {
                v.len() == 12 && v.bytes().all(|b| matches!(b, b'0'..=b'9'|b'a'..=b'f'))
            })
        {
            return Err(DistributionFailure::InvalidRecord);
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for BuildVersion {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).map_err(|_| "invalid version")
    }
}
impl From<BuildVersion> for String {
    fn from(value: BuildVersion) -> Self {
        value.0
    }
}
