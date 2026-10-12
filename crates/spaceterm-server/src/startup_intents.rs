//! Bounded, cancellable explicit-use choreography. Installed organization and
//! grants stay in the reducer. These transient entries never survive restart.

use crate::api::{ServerLimits, VerifiedPrincipal};
use crate::contracts::{ServerFailure, ServerFailureClass};
use crate::failure::{failure, startup_failure};
use spaceterm_core::{
    authority::*,
    control::*,
    geometry::GridLimits,
    identity::PaneId,
    operations::OperationId,
    startup_policy::{self, StartupContext},
};
use std::collections::BTreeMap;

pub struct MeasuredIntent {
    intent: StartupIntent,
    measurement: MeasuredStartupMeasurement,
}
impl MeasuredIntent {
    pub const fn intent(&self) -> StartupIntent {
        self.intent
    }
    pub fn reserve(self) -> Result<MeasuredStartupReservation, ServerFailure> {
        let old = self.intent.fence().session();
        let session = SessionKey {
            generation: old
                .generation
                .checked_successor()
                .ok_or_else(|| failure(ServerFailureClass::Exhausted))?,
            ..old
        };
        MeasuredStartupReservation::new(self.intent, self.measurement, session)
            .map_err(|_| failure(ServerFailureClass::StaleAuthority))
    }
}

pub struct StartupIntents {
    intents: BTreeMap<PaneId, StartupIntent>,
    limits: ServerLimits,
}
impl StartupIntents {
    pub fn new(limits: ServerLimits) -> Self {
        Self {
            intents: BTreeMap::new(),
            limits,
        }
    }
    pub fn begin(
        &mut self,
        principal: VerifiedPrincipal,
        intent: StartupIntent,
        current: StartupContext,
    ) -> Result<(), ServerFailure> {
        check_principal(principal, intent)?;
        startup_policy::validate_explicit_use(intent, current).map_err(startup_failure)?;
        if self.intents.get(&current.runtime.pane) == Some(&intent) {
            return Ok(());
        }
        if self.intents.contains_key(&current.runtime.pane) {
            return Err(failure(ServerFailureClass::Capacity));
        }
        let per_connection = self
            .intents
            .values()
            .filter(|v| v.fence().connection() == principal.binding())
            .count();
        if self.intents.len() >= self.limits.startup_intents
            || per_connection >= self.limits.startup_intents_per_connection
        {
            return Err(failure(ServerFailureClass::Capacity));
        }
        self.intents.insert(current.runtime.pane, intent);
        Ok(())
    }
    pub fn measure(
        &mut self,
        principal: VerifiedPrincipal,
        measurement: MeasuredStartupMeasurement,
        current: StartupContext,
        limits: GridLimits,
    ) -> Result<MeasuredIntent, ServerFailure> {
        let pane = measurement.fence.session().pane;
        let intent = *self
            .intents
            .get(&pane)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        check_principal(principal, intent)?;
        startup_policy::validate_measurement(intent, measurement, current, limits)
            .map_err(startup_failure)?;
        self.intents.remove(&pane);
        Ok(MeasuredIntent {
            intent,
            measurement,
        })
    }
    pub fn cancel(&mut self, id: OperationId) {
        self.intents.retain(|_, v| v.id() != id);
    }
    pub fn cancel_connection(&mut self, binding: ConnectionBinding) {
        self.intents
            .retain(|_, v| v.fence().connection() != binding);
    }
    pub fn cancel_pane(&mut self, pane: PaneId) {
        self.intents.remove(&pane);
    }
    pub fn cancel_all(&mut self) {
        self.intents.clear();
    }
}
fn check_principal(
    principal: VerifiedPrincipal,
    intent: StartupIntent,
) -> Result<(), ServerFailure> {
    if intent.id().stream().server() != principal.server()
        || intent.id().stream().pairing() != principal.pairing()
        || intent.id().stream().client() != principal.client()
        || intent.fence().connection() != principal.binding()
        || intent.fence().session().epoch != principal.epoch()
    {
        return Err(failure(ServerFailureClass::ScopeMismatch));
    }
    Ok(())
}
