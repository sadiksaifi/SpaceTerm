//! Network Access changes the service lane plans and applies. A plan reads the saved exposure,
//! the listener and the routes of live paired connections under the direct-network lock, so the
//! change it describes is the change `SetPolicy` makes in the same hold.
use super::*;
use crate::service::lane::network::{
    LaneCallerEffect, LaneNetworkSetting, ListenerProgress, NetworkAccess, NetworkCaller,
    NetworkPlan,
};
use std::collections::BTreeSet;
use std::net::SocketAddr;

/// Plans `access` for `caller` under `state`'s lock, and applies it in the same hold when
/// `decide` returns true.
pub(crate) fn change_network(
    state: &Shared,
    access: NetworkAccess,
    caller: Option<&NetworkCaller>,
    decide: &mut dyn FnMut(&NetworkPlan) -> Result<bool, ServerError>,
) -> Result<(), ServerError> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let (plan, target) = state.plan_network(access, caller)?;
    if decide(&plan)? {
        state.apply_network(target)?;
    }
    Ok(())
}

/// Whether `state`'s Network Access accepts connections now.
pub(crate) fn listener(state: &Shared) -> ListenerProgress {
    match state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .status()
        .listener
    {
        ListenerState::Listening { .. } => ListenerProgress::Listening,
        ListenerState::Starting { .. } => ListenerProgress::Starting,
        ListenerState::Off | ListenerState::Failed(_) => ListenerProgress::Stopped,
    }
}

/// What a plan applies: the policy `SetPolicy` saves and whether exposure is on.
struct Target {
    policy: Option<NetworkPolicy>,
    enabled: bool,
}

impl State {
    /// Plans `access` for `caller`, validated as `SetPolicy` validates it.
    fn plan_network(
        &mut self,
        access: NetworkAccess,
        caller: Option<&NetworkCaller>,
    ) -> Result<(NetworkPlan, Target), DirectError> {
        self.recover()?;
        let target = self.target(access)?;
        let keeps_listener = self.keeps_listener(&target);
        let serves = |route: Option<SocketAddr>| {
            keeps_listener
                || (target.enabled
                    && route.is_some_and(|route| {
                        target
                            .policy
                            .as_ref()
                            .is_some_and(|policy| policy.covers(route))
                    }))
        };
        let mut kept = BTreeSet::new();
        let mut lost = BTreeSet::new();
        for (pairing, route) in self.connections.routes(self.generation) {
            if serves(route) {
                kept.insert(pairing);
            } else {
                lost.insert(pairing);
            }
        }
        // A device keeps its access while any of its connections keeps a route.
        lost.retain(|pairing| !kept.contains(pairing));
        let disconnects = lost
            .iter()
            .filter_map(|pairing| {
                self.record
                    .devices
                    .iter()
                    .find(|device| device.info.pairing == *pairing && !device.revoked)
                    .map(|device| crate::service::lane::device(&device.info))
            })
            .collect();
        let caller = caller.map(|caller| {
            if keeps_listener {
                LaneCallerEffect::StaysConnected
            } else if serves(caller.route) {
                LaneCallerEffect::Reconnects
            } else {
                LaneCallerEffect::Disconnected
            }
        });
        let plan = NetworkPlan {
            from: setting(self.record.policy.as_ref(), self.record.enabled),
            to: setting(target.policy.as_ref(), target.enabled),
            generation: self.generation,
            // With exposure off no paired connection is live, so a new listener ends none.
            ends_connections: self.record.enabled && !keeps_listener,
            caller,
            disconnects,
            kept: kept.len(),
        };
        Ok((plan, target))
    }

    /// Applies a planned change through `SetPolicy`. A listener that failed afterwards fails
    /// the change, so its outcome is never reported as done.
    fn apply_network(&mut self, target: Target) -> Result<(), DirectError> {
        // Turning access off keeps the configured listener for the next deliberate enable.
        let policy = target.policy.filter(|_| target.enabled);
        let (reply, _) = self.manage(
            None,
            Management::SetPolicy {
                policy,
                enabled: target.enabled,
            },
        )?;
        match reply {
            ManagementReply::Status(DirectStatus {
                listener: ListenerState::Failed(error),
                ..
            }) => Err(error),
            ManagementReply::Status(_) => Ok(()),
            _ => Err(DirectError::InvalidRequest),
        }
    }

    fn target(&self, access: NetworkAccess) -> Result<Target, DirectError> {
        let current = self.record.policy.clone();
        let target = match access {
            NetworkAccess::Off => Target {
                policy: current,
                enabled: false,
            },
            NetworkAccess::All => Target {
                policy: Some(NetworkPolicy::every_interface(
                    current.map_or(DEFAULT_PORT, |policy| policy.bind.port()),
                )?),
                enabled: true,
            },
            NetworkAccess::Address(bind) => Target {
                policy: Some(NetworkPolicy::new(bind)?),
                enabled: true,
            },
        };
        // One address must belong to this machine. Every interface always does.
        if let Some(policy) = target.policy.as_ref().filter(|_| target.enabled)
            && !policy.is_every_interface()
            && !network::snapshot(self.network.as_ref())?.contains(&policy.bind.ip())
        {
            return Err(DirectError::InvalidAddress);
        }
        Ok(target)
    }

    /// Whether `SetPolicy` would keep the listener and its connections, as
    /// [`Self::prepare_listener`] decides. A widening that then fails to bind changes nothing.
    fn keeps_listener(&self, target: &Target) -> bool {
        let ListenerState::Listening { addr } = self.listener_state else {
            return false;
        };
        let Some(policy) = target.policy.as_ref().filter(|_| target.enabled) else {
            return false;
        };
        // A covering policy keeps this listener, or is refused when it can't widen.
        policy.covers(addr)
    }
}

fn setting(policy: Option<&NetworkPolicy>, enabled: bool) -> LaneNetworkSetting {
    LaneNetworkSetting {
        enabled,
        bind: policy.map(|policy| policy.bind.to_string()),
    }
}
