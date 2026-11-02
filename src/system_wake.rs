//! The system's wake from sleep, as one event SpaceTerm's models subscribe to. gpui reports the
//! wake to this source, which tests reach through [`deliver`].
use gpui::{App, AppContext as _, Entity, EventEmitter, Global};

pub(crate) struct SystemWake;

/// The system woke from sleep. Wall time moved while elapsed-time timers did not.
pub(crate) struct Woke;

impl EventEmitter<Woke> for SystemWake {}

struct Source(Entity<SystemWake>);

impl Global for Source {}

/// The wake source. The first call registers the platform's wake callback.
pub(crate) fn source(cx: &mut App) -> Entity<SystemWake> {
    if let Some(source) = cx.try_global::<Source>() {
        return source.0.clone();
    }
    let source = cx.new(|_| SystemWake);
    cx.on_system_wake(deliver).detach();
    cx.set_global(Source(source.clone()));
    source
}

/// Reports a wake from sleep to every subscriber.
pub(crate) fn deliver(cx: &mut App) {
    if let Some(source) = cx.try_global::<Source>().map(|source| source.0.clone()) {
        source.update(cx, |_, cx| cx.emit(Woke));
    }
}
