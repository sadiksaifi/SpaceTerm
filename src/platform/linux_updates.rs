//! Linux source builds carry no updater; every operation reports that updates are unavailable.
use crate::updates::{UpdateAdapter, UpdateError, UpdateEvent};

pub(super) struct LinuxUpdates;

impl UpdateAdapter for LinuxUpdates {
    fn start(&self, _: async_channel::Sender<UpdateEvent>) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn check(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn download(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn cancel(&self) {}
    fn install(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_updates_are_unavailable_for_every_operation() {
        let (sender, _receiver) = async_channel::unbounded();
        let updates = LinuxUpdates;
        assert_eq!(updates.start(sender), Err(UpdateError::Unavailable));
        assert_eq!(updates.check(), Err(UpdateError::Unavailable));
        assert_eq!(updates.download(), Err(UpdateError::Unavailable));
        assert_eq!(updates.install(), Err(UpdateError::Unavailable));
        assert_eq!(updates.finish_on_quit(), Err(UpdateError::Unavailable));
    }
}
