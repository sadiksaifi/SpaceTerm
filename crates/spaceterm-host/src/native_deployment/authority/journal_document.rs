//! Bounded current request and retained outcomes in the existing protected journal.
//! No entry is evicted: admission fails before effects when the reservation is full.

use serde::{Deserialize, Serialize};

use crate::{
    activation_journal::ActivationJournalRecord,
    native_deployment::{bindings, contracts::*},
};

const RETAINED_LIMIT: usize = 32;
const UNBOUND_LIMIT: usize = 16;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalDocument {
    schema_version: u16,
    pub(super) current: ActivationJournalRecord,
    retained: Vec<ActivationJournalRecord>,
    unbound_head_staging: Vec<String>,
    pub(super) repair: Option<super::repair::AdministrativeRepair>,
}
impl JournalDocument {
    pub(super) fn new(current: ActivationJournalRecord) -> Self {
        Self {
            schema_version: 1,
            current,
            retained: Vec::new(),
            unbound_head_staging: Vec::new(),
            repair: None,
        }
    }
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, NativeFailure> {
        // Existing single-request journals remain exact schema-one records. The storage
        // envelope is used only once retained entries or unbound publication inventory exist.
        let shape: serde_json::Value = bindings::decode_canonical(bytes, 64 * 1024)?;
        if shape.get("current").is_none() {
            return ActivationJournalRecord::decode(bytes).map(Self::new);
        }
        let document: Self = bindings::decode_canonical(bytes, 64 * 1024)?;
        document.validate()?;
        Ok(document)
    }
    pub(super) fn encode(&self) -> Result<Vec<u8>, NativeFailure> {
        self.validate()?;
        if self.retained.is_empty() && self.unbound_head_staging.is_empty() && self.repair.is_none()
        {
            self.current.encode()
        } else {
            bindings::canonical(self, 64 * 1024)
        }
    }
    fn validate(&self) -> Result<(), NativeFailure> {
        self.current.validate()?;
        if self.schema_version != 1 {
            return Err(NativeFailure::UnsupportedSchema);
        }
        if self.retained.len() > RETAINED_LIMIT || self.unbound_head_staging.len() > UNBOUND_LIMIT {
            return Err(NativeFailure::LimitExceeded);
        }
        if let Some(repair) = &self.repair {
            repair.validate(&self.current)?;
        }
        let mut ids = std::collections::BTreeSet::new();
        ids.insert(self.current.request.as_bytes());
        for record in &self.retained {
            record.validate()?;
            if record.outcome.is_none()
                || record.key != self.current.key
                || record.server_id != self.current.server_id
                || !ids.insert(record.request.as_bytes())
            {
                return Err(NativeFailure::InvalidRecord);
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for name in &self.unbound_head_staging {
            validate_staging_name(self.current.key, name)?;
            if !names.insert(name) {
                return Err(NativeFailure::InvalidRecord);
            }
        }
        Ok(())
    }
    pub(super) fn find(&self, request: NativeRequestId) -> Option<&ActivationJournalRecord> {
        std::iter::once(&self.current)
            .chain(&self.retained)
            .find(|record| record.request == request)
    }
    pub(super) fn replace(&mut self, next: &ActivationJournalRecord) -> Result<(), NativeFailure> {
        if self.current.request != next.request {
            if self.find(next.request).is_some() {
                return Err(NativeFailure::IdentityMismatch);
            }
            if self.current.outcome.is_none() {
                return Err(NativeFailure::OutcomePending);
            }
            if self.retained.len() == RETAINED_LIMIT {
                return Err(NativeFailure::LimitExceeded);
            }
            self.retained.push(self.current.clone());
        }
        self.current = next.clone();
        self.validate()
    }
    pub(super) fn reserve_outcome(&self, request: NativeRequestId) -> Result<(), NativeFailure> {
        if self.retained.len() == RETAINED_LIMIT || self.find(request).is_some() {
            return Err(NativeFailure::LimitExceeded);
        }
        Ok(())
    }
    pub(super) fn retain_outcome(
        &mut self,
        outcome: ActivationJournalRecord,
    ) -> Result<(), NativeFailure> {
        if let Some(previous) = self.find(outcome.request) {
            if *previous == outcome {
                return Ok(());
            }
            return Err(NativeFailure::IdentityMismatch);
        }
        self.reserve_outcome(outcome.request)?;
        self.retained.push(outcome);
        self.validate()
    }
    pub(super) fn preserve_unbound(&mut self, name: &str) -> Result<(), NativeFailure> {
        if !self
            .unbound_head_staging
            .iter()
            .any(|retained| retained == name)
        {
            if self.unbound_head_staging.len() == UNBOUND_LIMIT {
                return Err(NativeFailure::LimitExceeded);
            }
            self.unbound_head_staging.push(name.to_owned());
        }
        self.validate()
    }
    pub(super) fn require_staging_capacity(&self) -> Result<(), NativeFailure> {
        if self.unbound_head_staging.len() == UNBOUND_LIMIT {
            Err(NativeFailure::LimitExceeded)
        } else {
            Ok(())
        }
    }
}

pub(super) fn validate_staging_name(key: AccountBuildKey, name: &str) -> Result<(), NativeFailure> {
    let prefix = format!("{}.head-next-", super::records::prefix(key));
    if !name.starts_with(&prefix)
        || name.len() != prefix.len() + 32
        || !name[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(NativeFailure::InvalidRecord);
    }
    Ok(())
}
