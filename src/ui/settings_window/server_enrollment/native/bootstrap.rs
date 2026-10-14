//! Composes the foreground native provider. Reconnect retains only ordinary SSH routes.

use super::*;
use spaceterm_client_native::bootstrap::production::{
    ProductionBootstrapCompletion, ProductionSshBootstrap,
};

pub(crate) struct NativeForegroundBootstrap(pub(crate) ProductionSshBootstrap);
fn completion(value: ProductionBootstrapCompletion) -> BootstrapCompletion {
    BootstrapCompletion {
        prepared: value.prepared,
        saved: value.saved,
        outcome: value.outcome,
    }
}
impl ForegroundBootstrapServices for NativeForegroundBootstrap {
    fn inspect(
        &self,
        host: String,
        attempt: EnrollmentAttempt,
    ) -> PairingFuture<'_, Result<(BootstrapHostFacts, Option<BootstrapRequest>), EnrollmentFailure>>
    {
        Box::pin(async move {
            attempt.validate()?;
            let (facts, pending) = self
                .0
                .inspect_with_pending(host, attempt.generation())
                .await?;
            if let Err(error) = attempt.validate() {
                self.0.abandon(facts.route, attempt.generation())?;
                return Err(error);
            }
            self.0
                .bind_foreground(facts.route, attempt.generation(), attempt.valid.clone())?;
            Ok((
                facts,
                pending
                    .map(|(request, artifact)| BootstrapRequest::new(request, artifact))
                    .transpose()?,
            ))
        })
    }
    fn bootstrap(
        &self,
        intent: DeliberateBootstrap,
        candidate: SshCandidate,
    ) -> PairingFuture<'_, Result<BootstrapCompletion, EnrollmentFailure>> {
        Box::pin(async move {
            candidate.attempt.validate()?;
            self.0.bind_foreground(
                candidate.facts.route,
                candidate.attempt.generation(),
                candidate.attempt.valid.clone(),
            )?;
            let result = self.0.bootstrap(intent, candidate.facts()).await?;
            candidate.attempt.validate()?;
            Ok(completion(result))
        })
    }
    fn resolve(
        &self,
        candidate: SshCandidate,
        request: BootstrapRequest,
    ) -> PairingFuture<'_, Result<BootstrapCompletion, EnrollmentFailure>> {
        Box::pin(async move {
            candidate.attempt.validate()?;
            self.0.bind_foreground(
                candidate.facts.route,
                candidate.attempt.generation(),
                candidate.attempt.valid.clone(),
            )?;
            let result = self
                .0
                .resolve(
                    candidate.facts(),
                    candidate.attempt.generation(),
                    request.request(),
                    request.artifact(),
                )
                .await?;
            candidate.attempt.validate()?;
            Ok(completion(result))
        })
    }
    fn start(&self, candidate: SshCandidate) -> PairingFuture<'_, Result<(), EnrollmentFailure>> {
        Box::pin(async move {
            candidate.attempt.validate()?;
            self.0.bind_foreground(
                candidate.facts.route,
                candidate.attempt.generation(),
                candidate.attempt.valid.clone(),
            )?;
            self.0
                .start(candidate.facts(), candidate.attempt.generation())
                .await?;
            candidate.attempt.validate()
        })
    }
}
