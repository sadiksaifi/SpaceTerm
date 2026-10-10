//! Application-owned Background Image presentation for every Workspace window.
//!
//! The runtime loads the copy the previewed Settings name off the UI thread, so a Workspace
//! window shows a changed image while the Settings Window is still saving it. When the committed
//! Settings stop naming a copy, the runtime discards it, unless the preview still names it.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use gpui::{App, AppContext as _, BorrowAppContext as _, Global, Task};

use crate::background_image::{BackgroundImageId, BackgroundImageStore};
use crate::settings::Settings;

/// The loaded Background Image a Workspace window presents.
#[derive(Clone)]
pub(crate) struct PresentedBackgroundImage {
    pub(crate) id: BackgroundImageId,
    pub(crate) bytes: Arc<[u8]>,
}

pub(crate) struct BackgroundImageRuntime {
    settings: Settings,
    store: Arc<BackgroundImageStore>,
    /// The copy the previewed Settings name, loaded or loading.
    requested: Option<BackgroundImageId>,
    presented: Option<PresentedBackgroundImage>,
    /// The copy the committed Settings name, which the runtime keeps until they stop naming it.
    committed: Option<BackgroundImageId>,
    load: Option<Task<()>>,
    _subscription: Task<()>,
}
impl Global for BackgroundImageRuntime {}

pub(crate) fn install(settings: Settings, store: Arc<BackgroundImageStore>, cx: &mut App) {
    let changed = settings.subscribe();
    let subscription = cx.spawn(async move |cx| {
        while changed.recv().await.is_ok() {
            while changed.try_recv().is_ok() {}
            cx.update(sync);
        }
    });
    let committed = settings
        .snapshot()
        .committed
        .appearance
        .window
        .background_image;
    cx.set_global(BackgroundImageRuntime {
        settings,
        store,
        requested: None,
        presented: None,
        committed,
        load: None,
        _subscription: subscription,
    });
    sync(cx);
}

/// The image Workspace windows present now.
pub(crate) fn presented(cx: &App) -> Option<PresentedBackgroundImage> {
    cx.try_global::<BackgroundImageRuntime>()
        .and_then(|runtime| runtime.presented.clone())
}

/// The store that copies a chosen image, present only where windows can present one.
pub(crate) fn store(cx: &App) -> Option<Arc<BackgroundImageStore>> {
    cx.try_global::<BackgroundImageRuntime>()
        .map(|runtime| Arc::clone(&runtime.store))
}

fn sync(cx: &mut App) {
    let runtime = cx.global_mut::<BackgroundImageRuntime>();
    let snapshot = runtime.settings.snapshot();
    let store = Arc::clone(&runtime.store);
    let requested = snapshot.candidate.appearance.window.background_image;
    let committed = snapshot.committed.appearance.window.background_image;
    let retired = std::mem::replace(&mut runtime.committed, committed)
        .filter(|previous| Some(*previous) != committed && Some(*previous) != requested);
    let request_changed = runtime.requested != requested;
    runtime.requested = requested;
    if let Some(retired) = retired {
        let store = Arc::clone(&store);
        cx.background_spawn(async move {
            let _ = store.discard(retired);
        })
        .detach();
    }
    if !request_changed {
        return;
    }
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
                    runtime.presented = loaded
                        .ok()
                        .map(|bytes| PresentedBackgroundImage { id, bytes });
                }
            });
        });
    });
    cx.global_mut::<BackgroundImageRuntime>().load = Some(load);
}
