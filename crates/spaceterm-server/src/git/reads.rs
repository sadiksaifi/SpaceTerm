//! Principal-bound native assessment cache. Presentation never defines authorization scope.
use super::authority::*;
use super::host::GitHostAssessment;
use super::writes::GitService;
use spaceterm_core::worktrees::*;
use std::sync::Arc;
use std::time::Instant;
pub(crate) struct CachedAssessment {
    pub token: GitAssessmentToken,
    pub principal: AuthenticatedGitPrincipal,
    pub plan: ConfirmedGitPlan,
    pub native: Arc<dyn GitHostAssessment>,
    pub created: Instant,
    bytes: usize,
}
impl GitService {
    /// Internal host/actor query pipeline only. Wire callers cannot inject plans or native proofs.
    pub async fn cache_assessment(
        &self,
        principal: AuthenticatedGitPrincipal,
        plan: ConfirmedGitPlan,
        native: Arc<dyn GitHostAssessment>,
    ) -> Result<GitAssessmentToken, GitFailure> {
        self.check_principal(principal)?;
        plan.validate_scope()?;
        self.admission()?;
        self.inner.organization.authorize(principal).await?;
        if plan.repository.scope().server != self.inner.server {
            return Err(GitFailure::WrongServer);
        }
        let bytes = match &plan.action {
            ConfirmedGitAction::Create { preview, .. } => {
                2048 + preview.parts().directory.bytes().len()
                    + plan.repository.parts().main_root.bytes().len()
                    + plan.repository.parts().common_directory.bytes().len()
            }
            ConfirmedGitAction::Remove(scope) => {
                2048 + scope.retained().parts().tabs.len() * 128
                    + scope.retained().parts().panes.len() * (256 + 4096)
                    + scope
                        .retained()
                        .parts()
                        .fallbacks
                        .iter()
                        .map(|f| 128 + f.directory.bytes().len())
                        .sum::<usize>()
            }
        };
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(|_| GitFailure::Unchecked)?;
        let token = GitAssessmentToken::new(GitAssessmentTokenParts {
            server: self.inner.server,
            epoch: self.inner.epoch,
            pairing: principal.pairing(),
            client: principal.client(),
            kind: plan.kind(),
            random,
        })?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if !state.ready || state.closing || state.uncertain {
            return Err(GitFailure::Busy);
        }
        state
            .assessments
            .retain(|_, entry| entry.created.elapsed().as_secs() < 120);
        if state.assessments.len() >= self.inner.limits.assessments
            || state
                .assessments
                .values()
                .filter(|entry| entry.principal == principal)
                .count()
                >= self.inner.limits.assessments_per_client
            || state
                .assessments
                .values()
                .map(|entry| entry.bytes)
                .sum::<usize>()
                + bytes
                > 16 * 1024 * 1024
        {
            return Err(GitFailure::Capacity);
        }
        if state.assessments.contains_key(&random) {
            return Err(GitFailure::Unchecked);
        }
        state.assessments.insert(
            random,
            CachedAssessment {
                token: token.clone(),
                principal,
                plan,
                native,
                created: Instant::now(),
                bytes,
            },
        );
        Ok(token)
    }
}
