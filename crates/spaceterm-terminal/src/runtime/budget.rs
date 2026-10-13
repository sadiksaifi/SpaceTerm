//! Count, byte and grid-plane reservations shared by one runtime factory.
use super::ports::{RuntimeFailure, RuntimeStage};
use spaceterm_core::geometry::{GridLimits, MeasuredGrid};
use spaceterm_core::identity::PaneId;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub const REPLY_INPUT_RESERVE: usize = 64 * 1024;
#[derive(Clone, Copy, Debug)]
pub struct RuntimeLimits {
    pub grid: GridLimits,
    pub active_grid_slots: usize,
    pub owners: usize,
    pub commands: usize,
    pub command_bytes: usize,
    pub aggregate_command_bytes: usize,
    pub pending_input_bytes: usize,
    pub aggregate_pending_input_bytes: usize,
    pub query_scratch_bytes: usize,
    pub content_jobs: usize,
    pub subscriptions: usize,
}
impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            grid: GridLimits::new(512, 1024, 524_288)
                .unwrap_or(spaceterm_core::geometry::SUPPORTED_GRID_LIMITS),
            active_grid_slots: 8_388_608,
            owners: 64,
            commands: 256,
            command_bytes: 4 * 1024 * 1024,
            aggregate_command_bytes: 64 * 1024 * 1024,
            pending_input_bytes: 4 * 1024 * 1024,
            aggregate_pending_input_bytes: 64 * 1024 * 1024,
            query_scratch_bytes: 64 * 1024 * 1024,
            content_jobs: 8,
            subscriptions: 16,
        }
    }
}
impl RuntimeLimits {
    pub fn validate(self) -> Result<Self, RuntimeFailure> {
        let hard = Self::default();
        if self.grid.max_rows() > hard.grid.max_rows()
            || self.grid.max_columns() > hard.grid.max_columns()
            || self.grid.max_cells() > hard.grid.max_cells()
            || self.active_grid_slots == 0
            || self.active_grid_slots > hard.active_grid_slots
            || self.owners == 0
            || self.owners > hard.owners
            || self.commands == 0
            || self.commands > hard.commands
            || self.command_bytes == 0
            || self.command_bytes > hard.command_bytes
            || self.aggregate_command_bytes == 0
            || self.aggregate_command_bytes > hard.aggregate_command_bytes
            || self.pending_input_bytes <= REPLY_INPUT_RESERVE
            || self.pending_input_bytes > hard.pending_input_bytes
            || self.aggregate_pending_input_bytes <= self.owners * REPLY_INPUT_RESERVE
            || self.aggregate_pending_input_bytes > hard.aggregate_pending_input_bytes
            || self.query_scratch_bytes == 0
            || self.query_scratch_bytes > hard.query_scratch_bytes
            || self.content_jobs == 0
            || self.content_jobs > hard.content_jobs
            || self.subscriptions == 0
            || self.subscriptions > hard.subscriptions
        {
            return Err(RuntimeFailure::capacity(RuntimeStage::Admission));
        }
        Ok(self)
    }
}
#[derive(Default)]
struct Usage {
    count: usize,
    bytes: usize,
}
pub(crate) struct Budget {
    usage: Mutex<Usage>,
    count: usize,
    bytes: usize,
}
impl Budget {
    pub(crate) fn new(count: usize, bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            usage: Mutex::new(Usage::default()),
            count,
            bytes,
        })
    }
    pub(crate) fn reserve(
        self: &Arc<Self>,
        count: usize,
        bytes: usize,
        stage: RuntimeStage,
    ) -> Result<BudgetLease, RuntimeFailure> {
        let mut usage = self
            .usage
            .lock()
            .map_err(|_| RuntimeFailure::internal(stage))?;
        let next_count = usage
            .count
            .checked_add(count)
            .filter(|value| *value <= self.count)
            .ok_or(RuntimeFailure::capacity(stage))?;
        let next_bytes = usage
            .bytes
            .checked_add(bytes)
            .filter(|value| *value <= self.bytes)
            .ok_or(RuntimeFailure::capacity(stage))?;
        usage.count = next_count;
        usage.bytes = next_bytes;
        Ok(BudgetLease {
            claims: vec![Claim {
                budget: Arc::clone(self),
                count,
                bytes,
            }],
        })
    }
}
struct Claim {
    budget: Arc<Budget>,
    count: usize,
    bytes: usize,
}
impl Drop for Claim {
    fn drop(&mut self) {
        let mut usage = self
            .budget
            .usage
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        usage.count = usage.count.saturating_sub(self.count);
        usage.bytes = usage.bytes.saturating_sub(self.bytes);
    }
}
/// A non-cloneable residency claim. Content providers keep it through allocation use.
#[derive(Default)]
pub struct BudgetLease {
    claims: Vec<Claim>,
}
impl BudgetLease {
    pub fn bytes(&self) -> usize {
        self.claims.first().map_or(0, |claim| claim.bytes)
    }
    pub(crate) fn combine(mut self, mut other: Self) -> Self {
        self.claims.append(&mut other.claims);
        self
    }
    /// Keep the usage charged while dropping the owning Arc and claim storage.
    pub(crate) fn quarantine(mut self) {
        for claim in &mut self.claims {
            claim.count = 0;
            claim.bytes = 0;
        }
    }
    pub(crate) fn shrink(&mut self, bytes: usize) {
        for claim in &mut self.claims {
            let released = claim.bytes.saturating_sub(bytes);
            let mut usage = claim
                .budget
                .usage
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            usage.bytes = usage.bytes.saturating_sub(released);
            claim.bytes -= released;
        }
    }
}
pub(crate) struct FactoryBudget {
    pub limits: RuntimeLimits,
    panes: Mutex<HashSet<PaneId>>,
    pub grid: Arc<Budget>,
    pub ingress: Arc<Budget>,
    pub input: Arc<Budget>,
    pub client_input: Arc<Budget>,
    pub query: Arc<Budget>,
}
impl FactoryBudget {
    pub fn new(limits: RuntimeLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            panes: Mutex::new(HashSet::new()),
            grid: Budget::new(0, limits.active_grid_slots),
            ingress: Budget::new(0, limits.aggregate_command_bytes),
            input: Budget::new(0, limits.aggregate_pending_input_bytes),
            client_input: Budget::new(
                0,
                limits.aggregate_pending_input_bytes - limits.owners * REPLY_INPUT_RESERVE,
            ),
            query: Budget::new(0, limits.query_scratch_bytes),
        })
    }
    pub fn register(
        self: &Arc<Self>,
        pane: PaneId,
        grid: MeasuredGrid,
    ) -> Result<RuntimeRegistration, RuntimeFailure> {
        grid.validate(self.limits.grid)
            .map_err(|_| RuntimeFailure::capacity(RuntimeStage::Admission))?;
        let mut panes = self
            .panes
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Admission))?;
        if panes.len() >= self.limits.owners || panes.contains(&pane) {
            return Err(RuntimeFailure::capacity(RuntimeStage::Admission));
        }
        let lease = self
            .grid
            .reserve(0, steady_slots(grid)?, RuntimeStage::Admission)?;
        panes.insert(pane);
        Ok(RuntimeRegistration {
            factory: Arc::clone(self),
            pane,
            grid,
            lease: Some(lease),
            release_pane: true,
        })
    }
}
pub(crate) fn steady_slots(grid: MeasuredGrid) -> Result<usize, RuntimeFailure> {
    (grid.cells() as usize)
        .checked_mul(4)
        .ok_or(RuntimeFailure::capacity(RuntimeStage::Resize))
}
pub(crate) struct RuntimeRegistration {
    pub factory: Arc<FactoryBudget>,
    pane: PaneId,
    pub grid: MeasuredGrid,
    lease: Option<BudgetLease>,
    release_pane: bool,
}
impl RuntimeRegistration {
    pub fn reserve_resize(
        &self,
        next: MeasuredGrid,
    ) -> Result<Option<BudgetLease>, RuntimeFailure> {
        next.validate(self.factory.limits.grid)
            .map_err(|_| RuntimeFailure::capacity(RuntimeStage::Resize))?;
        if next.rows() == self.grid.rows() && next.columns() == self.grid.columns() {
            return Ok(None);
        }
        let slots = steady_slots(next)?
            .checked_add(
                (self.grid.cells() as usize)
                    .checked_mul(2)
                    .ok_or(RuntimeFailure::capacity(RuntimeStage::Resize))?,
            )
            .ok_or(RuntimeFailure::capacity(RuntimeStage::Resize))?;
        self.factory
            .grid
            .reserve(0, slots, RuntimeStage::Resize)
            .map(Some)
    }
    pub fn commit_resize(
        &mut self,
        next: MeasuredGrid,
        temporary: Option<BudgetLease>,
    ) -> Result<(), RuntimeFailure> {
        if let Some(mut lease) = temporary {
            lease.shrink(steady_slots(next)?);
            self.lease = Some(lease);
        }
        self.grid = next;
        Ok(())
    }
    pub fn quarantine(&mut self) {
        self.release_pane = false;
    }
    pub fn confirm_cleanup(&mut self) {
        self.release_pane = true;
    }
}
impl Drop for RuntimeRegistration {
    fn drop(&mut self) {
        if self.release_pane {
            self.factory
                .panes
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&self.pane);
        } else if let Some(lease) = self.lease.take() {
            lease.quarantine();
        }
    }
}
