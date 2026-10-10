//! Application-owned Background Image presentation for every Workspace window.
//!
//! The runtime loads the copy the previewed Settings name off the UI thread, so a Workspace
//! window shows a changed image while the Settings Window is still saving it. It owns every copy
//! a Settings Document, a lease, or a choice has named, and discards a copy once no retained or
//! pending document and no lease names it. While a choice is still copying an image, nothing is
//! discarded. A discard that lapsed or failed leaves the copy owned, to be tried again.

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gpui::{App, AppContext as _, BorrowAppContext as _, Global, Task};

use crate::background_image::{
    BackgroundImageId, BackgroundImageStore, Discarded, LoadedBackgroundImage,
};
use crate::settings::Settings;

pub(crate) struct BackgroundImageRuntime {
    settings: Settings,
    store: Arc<BackgroundImageStore>,
    /// The copy the previewed Settings name, loaded or loading.
    requested: Option<BackgroundImageId>,
    presented: Option<LoadedBackgroundImage>,
    /// The copies this runtime discards once nothing names them.
    owned: HashSet<BackgroundImageId>,
    /// Copies named by documents that have not reached the Settings yet, such as an editor's
    /// draft waiting for another write to finish.
    leases: Vec<Weak<BackgroundImageId>>,
    /// Choices still copying an image, which may name a copy this runtime is about to discard.
    choosing: usize,
    load: Option<Task<()>>,
    _subscription: Task<()>,
}
impl Global for BackgroundImageRuntime {}

/// Keeps one copy while it lives. Dropping it lets the next change discard the copy, unless
/// something else names it.
pub(crate) struct BackgroundImageLease(Rc<BackgroundImageId>);

impl BackgroundImageLease {
    pub(crate) fn id(&self) -> BackgroundImageId {
        *self.0
    }
}

pub(crate) fn install(settings: Settings, store: Arc<BackgroundImageStore>, cx: &mut App) {
    let changed = settings.subscribe();
    let subscription = cx.spawn(async move |cx| {
        while changed.recv().await.is_ok() {
            while changed.try_recv().is_ok() {}
            cx.update(|cx| sync(false, cx));
        }
    });
    cx.set_global(BackgroundImageRuntime {
        settings,
        store,
        requested: None,
        presented: None,
        owned: HashSet::new(),
        leases: Vec::new(),
        choosing: 0,
        load: None,
        _subscription: subscription,
    });
    sync(false, cx);
}

/// The image Workspace windows present now.
pub(crate) fn presented(cx: &App) -> Option<LoadedBackgroundImage> {
    cx.try_global::<BackgroundImageRuntime>()
        .and_then(|runtime| runtime.presented.clone())
}

/// The store that copies a chosen image, present only where windows can present one.
pub(crate) fn store(cx: &App) -> Option<Arc<BackgroundImageStore>> {
    cx.try_global::<BackgroundImageRuntime>()
        .map(|runtime| Arc::clone(&runtime.store))
}

/// Keeps the copy `id` names while the returned lease lives.
pub(crate) fn lease(id: BackgroundImageId, cx: &mut App) -> Option<BackgroundImageLease> {
    cx.try_global::<BackgroundImageRuntime>()?;
    let lease = Rc::new(id);
    let runtime = cx.global_mut::<BackgroundImageRuntime>();
    runtime.leases.push(Rc::downgrade(&lease));
    if runtime.owned.insert(id) {
        runtime.store.renew();
    }
    Some(BackgroundImageLease(lease))
}

/// Holds every discard until [`end_choice`], because the image being copied may be one this
/// runtime would otherwise discard before the Settings name it again.
pub(crate) fn begin_choice(cx: &mut App) {
    if let Some(runtime) = cx.try_global::<BackgroundImageRuntime>() {
        let choosing = runtime.choosing + 1;
        cx.global_mut::<BackgroundImageRuntime>().choosing = choosing;
    }
}

/// Ends a choice. The runtime owns the copy it kept, and discards it unless something names it.
/// A copy that the Settings already named but that failed to load is loaded again.
pub(crate) fn end_choice(kept: Option<BackgroundImageId>, cx: &mut App) {
    if cx.try_global::<BackgroundImageRuntime>().is_none() {
        return;
    }
    let runtime = cx.global_mut::<BackgroundImageRuntime>();
    runtime.choosing = runtime.choosing.saturating_sub(1);
    let reload = kept.is_some_and(|id| {
        runtime.owned.insert(id);
        runtime.requested == Some(id)
            && runtime.presented.as_ref().map(|image| image.id) != Some(id)
    });
    sync(reload, cx);
}

fn sync(reload: bool, cx: &mut App) {
    let runtime = cx.global_mut::<BackgroundImageRuntime>();
    let store = Arc::clone(&runtime.store);
    let requested = runtime
        .settings
        .snapshot()
        .candidate
        .appearance
        .window
        .background_image;
    runtime.leases.retain(|lease| lease.strong_count() > 0);
    let named = runtime
        .settings
        .retainable_documents()
        .iter()
        .filter_map(|document| document.appearance.window.background_image)
        .chain(
            runtime
                .leases
                .iter()
                .filter_map(|lease| lease.upgrade().map(|id| *id)),
        )
        .collect::<HashSet<_>>();
    // A copy named again, by an import or an edit to the settings file, lapses any discard of it
    // that is still waiting to run.
    if named.iter().any(|id| !runtime.owned.contains(id)) {
        store.renew();
    }
    runtime.owned.extend(&named);
    if runtime.choosing == 0 {
        let retired = runtime
            .owned
            .iter()
            .filter(|id| !named.contains(id))
            .map(|&id| store.retire(id))
            .collect::<Vec<_>>();
        runtime.owned.retain(|id| named.contains(id));
        if !retired.is_empty() {
            let store = Arc::clone(&store);
            let discarded = cx.background_spawn(async move {
                let mut lapsed = Vec::new();
                let mut failed = Vec::new();
                for retirement in retired {
                    match store.discard(retirement) {
                        Ok(Discarded::Removed) => {}
                        Ok(Discarded::Lapsed) => lapsed.push(retirement.id()),
                        Err(_) => failed.push(retirement.id()),
                    }
                }
                (lapsed, failed)
            });
            // A lapsed discard is decided again at once against what is named now. A failed one
            // waits for the next change, so a lasting failure cannot spin.
            cx.spawn(async move |cx| {
                let (lapsed, failed) = discarded.await;
                if lapsed.is_empty() && failed.is_empty() {
                    return;
                }
                cx.update(|cx| {
                    let runtime = cx.global_mut::<BackgroundImageRuntime>();
                    runtime.owned.extend(failed);
                    if !lapsed.is_empty() {
                        runtime.owned.extend(lapsed);
                        sync(false, cx);
                    }
                });
            })
            .detach();
        }
    }
    let runtime = cx.global_mut::<BackgroundImageRuntime>();
    if runtime.requested == requested && !reload {
        return;
    }
    runtime.requested = requested;
    let Some(id) = requested else {
        cx.update_global::<BackgroundImageRuntime, _>(|runtime, _| {
            runtime.load = None;
            runtime.presented = None;
        });
        return;
    };
    // The previous image stays until this one loads, so changing images never flashes the
    // desktop.
    let loaded = cx.background_spawn(async move { store.load(id) });
    let load = cx.spawn(async move |cx| {
        let loaded = loaded.await;
        cx.update(|cx| {
            cx.update_global::<BackgroundImageRuntime, _>(|runtime, _| {
                if runtime.requested == Some(id) {
                    runtime.presented =
                        loaded.ok().map(|bytes| LoadedBackgroundImage { id, bytes });
                }
            });
        });
    });
    cx.global_mut::<BackgroundImageRuntime>().load = Some(load);
}
