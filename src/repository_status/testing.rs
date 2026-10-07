//! Test doubles for the Repository Status adapter boundary.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::{ProgramError, ProgramExit, ProgramRequest, RepositoryProgramRunner};
use crate::ssh::cancellation::SshCancellationToken;

/// An exit status and the stdout chunks delivered before it, or a runner failure.
pub(super) type FakeResponse = Result<(Option<i32>, Vec<Vec<u8>>), ProgramError>;

/// Records every request and answers from a script, delivering stdout in the given chunks.
#[derive(Default)]
pub(super) struct FakeRunner {
    responses: Mutex<VecDeque<FakeResponse>>,
    requests: Mutex<Vec<ProgramRequest>>,
}

impl FakeRunner {
    pub(super) fn new(responses: impl IntoIterator<Item = FakeResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::default(),
        })
    }

    pub(super) fn requests(&self) -> Vec<ProgramRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl RepositoryProgramRunner for FakeRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        _cancellation: &SshCancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        self.requests.lock().unwrap().push(request.clone());
        let (code, chunks) = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("an unscripted program run")?;
        for chunk in chunks {
            stdout(&chunk);
        }
        Ok(ProgramExit { code })
    }
}

pub(super) fn exit(code: i32, stdout: &str) -> FakeResponse {
    Ok((Some(code), vec![stdout.as_bytes().to_vec()]))
}
