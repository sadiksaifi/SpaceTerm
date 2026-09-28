//! Development-only update transport. All events are synthetic; no installer or network is used.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{UpdateAdapter, UpdateError, UpdateEvent};

// Display fixture, never a release version authority.
const PREVIEW_VERSION: &str = "0.1.1";
const STEP: Duration = Duration::from_millis(400);

#[derive(Clone, Copy)]
pub(crate) enum Scenario {
    Available,
    UpToDate,
    CheckError,
    DownloadError,
    VerificationError,
    InstallError,
}

impl Scenario {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "available" => Self::Available,
            "up-to-date" => Self::UpToDate,
            "check-error" => Self::CheckError,
            "download-error" => Self::DownloadError,
            "verification-error" => Self::VerificationError,
            "install-error" => Self::InstallError,
            _ => return None,
        })
    }
}

#[derive(Default)]
struct Run {
    generation: u64,
    events: Option<async_channel::Sender<UpdateEvent>>,
}

pub(crate) struct PreviewUpdates {
    scenario: Scenario,
    run: Arc<Mutex<Run>>,
}

impl PreviewUpdates {
    pub(crate) fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            run: Arc::new(Mutex::new(Run::default())),
        }
    }

    fn play(&self, events: Vec<(Duration, UpdateEvent)>) -> Result<(), UpdateError> {
        let generation = {
            let mut run = self.run.lock().map_err(|_| UpdateError::Unavailable)?;
            run.generation = run.generation.wrapping_add(1);
            run.generation
        };
        let run = Arc::clone(&self.run);
        std::thread::Builder::new()
            .name("update-preview".into())
            .spawn(move || {
                for (delay, event) in events {
                    std::thread::sleep(delay);
                    let Ok(run) = run.lock() else { return };
                    if run.generation != generation {
                        return;
                    }
                    let Some(sender) = &run.events else { return };
                    if sender.try_send(event).is_err() {
                        return;
                    }
                }
            })
            .map_err(|_| UpdateError::Unavailable)?;
        Ok(())
    }
}

impl UpdateAdapter for PreviewUpdates {
    fn start(&self, sender: async_channel::Sender<UpdateEvent>) -> Result<(), UpdateError> {
        self.run
            .lock()
            .map_err(|_| UpdateError::Unavailable)?
            .events = Some(sender);
        Ok(())
    }

    fn check(&self) -> Result<(), UpdateError> {
        let events = match self.scenario {
            Scenario::UpToDate => vec![
                (STEP, UpdateEvent::UpToDate),
                (Duration::ZERO, UpdateEvent::Finished),
            ],
            Scenario::CheckError => vec![
                (STEP, UpdateEvent::Failed(UpdateError::Check)),
                (Duration::ZERO, UpdateEvent::Finished),
            ],
            _ => vec![(STEP, UpdateEvent::Available(PREVIEW_VERSION.into()))],
        };
        self.play(events)
    }

    fn download(&self) -> Result<(), UpdateError> {
        let mut events = Vec::new();
        for received in 0..=12 {
            events.push((
                STEP,
                UpdateEvent::Downloading {
                    received,
                    total: 12,
                },
            ));
            if received == 5 && matches!(self.scenario, Scenario::DownloadError) {
                events.push((STEP, UpdateEvent::Failed(UpdateError::Download)));
                events.push((Duration::ZERO, UpdateEvent::Finished));
                return self.play(events);
            }
        }
        events.push((STEP, UpdateEvent::Verifying));
        if matches!(self.scenario, Scenario::VerificationError) {
            events.push((STEP * 3, UpdateEvent::Failed(UpdateError::Verification)));
            events.push((Duration::ZERO, UpdateEvent::Finished));
        } else {
            events.push((STEP * 3, UpdateEvent::Ready));
        }
        self.play(events)
    }

    fn cancel(&self) {
        if let Ok(mut run) = self.run.lock() {
            // Send Finished under the same lock as progress so no cancelled worker can emit later.
            run.generation = run.generation.wrapping_add(1);
            if let Some(sender) = &run.events {
                let _ = sender.try_send(UpdateEvent::Finished);
            }
        }
    }

    fn install(&self) -> Result<(), UpdateError> {
        let result = if matches!(self.scenario, Scenario::InstallError) {
            UpdateEvent::Failed(UpdateError::Installation)
        } else {
            // The confirmation can be exercised safely; completion only hides the preview control.
            UpdateEvent::UpToDate
        };
        self.play(vec![
            (STEP, result),
            (Duration::ZERO, UpdateEvent::Finished),
        ])
    }
}

impl Drop for PreviewUpdates {
    fn drop(&mut self) {
        self.cancel();
    }
}
