//! Cleanup authority for a disposable Workspace, fenced by the server boot that created it.
use spaceterm_server::{
    ServerConnection,
    connection::link::ServerLink,
    hierarchy::{CloseTarget, Id, Mutation, Outcome},
};
use std::error::Error;

pub struct Workspace<'a> {
    client: &'a ServerConnection,
    boot: Id,
    workspace: Option<Id>,
    pub pane: Id,
}
impl<'a> Workspace<'a> {
    /// Tracks only a Workspace this exercise just created, fenced by the server's boot.
    pub fn track(
        client: &'a ServerConnection,
        link: &ServerLink,
        workspace: Id,
    ) -> Result<Self, Box<dyn Error>> {
        let (inventory, _) = super::support::inventory(link)?;
        let boot = inventory.close.as_ref().ok_or("server boot missing")?.boot;
        let pane = inventory
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .and_then(|w| w.tabs.first())
            .ok_or("created Workspace missing")?
            .root_pane;
        Ok(Self {
            client,
            boot,
            workspace: Some(workspace),
            pane,
        })
    }

    pub fn cleanup(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(workspace) = self.workspace else {
            return Ok(());
        };
        let link = super::support::connect(self.client)?;
        let (inventory, next_sequence) = super::support::inventory(&link)?;
        if inventory.close.as_ref().map(|live| live.boot) != Some(self.boot) {
            return Err("cleanup refused a replacement server".into());
        }
        if inventory.workspaces.iter().any(|w| w.id == workspace) {
            let target = CloseTarget::KillWorkspace { workspace };
            let confirmation = inventory
                .close_confirmation(target)
                .ok_or("cleanup authority missing")?;
            let result = super::support::mutate(
                &link,
                next_sequence,
                Mutation::Close {
                    target,
                    confirmation,
                },
            )?;
            if result.outcome != (Outcome::Accepted { target: workspace }) {
                return Err("test Workspace cleanup refused".into());
            }
        }
        self.workspace = None;
        Ok(())
    }
}
impl Drop for Workspace<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("test Workspace cleanup failed: {error}");
        }
    }
}
