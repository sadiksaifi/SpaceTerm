//! Application-owned Background Image presentation for every Workspace window.
//!
//! The runtime loads the copy the previewed Settings name off the UI thread, so a Workspace
//! window shows a changed image while the Settings Window is still saving it. It owns every copy
//! a Settings Document has named or a choice has kept, and discards a copy once no retained or
//! pending document names it. While a choice is still copying an image, nothing is discarded.

#[cfg(test)]
mod tests;

use std::collections::HashSet;
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
    /// The copies this runtime discards once nothing names them.
    owned: HashSet<BackgroundImageId>,
    /// Choices still copying an image, which may name a copy this runtime is about to discard.
    choosing: usize,
    load: Option<Task<()>>,
    _subscription: Task<()>,
}
impl Global for BackgroundImageRuntime {}

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
        choosing: 0,
        load: None,
        _subscription: subscription,
    });
    sync(false, cx);
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

/// Holds every discard until [`end_choice`], because the image being copied may be one this
/// runtime would otherwise discard before the Settings name it again.
pub(crate) fn begin_choice(cx: &mut App) {
    if let Some(runtime) = cx.try_global::<BackgroundImageRuntime>() {
        let choosing = runtime.choosing + 1;
        cx.global_mut::<BackgroundImageRuntime>().choosing = choosing;
    }
}

/// Ends a choice. The runtime owns the copy it kept, and discards it unless the Settings name it.
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
    let named = runtime
        .settings
        .retainable_documents()
        .iter()
        .filter_map(|document| document.appearance.window.background_image)
        .collect::<HashSet<_>>();
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
            cx.background_spawn(async move {
                for retirement in retired {
                    let _ = store.discard(retirement);
                }
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
                    runtime.presented = loaded
                        .ok()
                        .map(|bytes| PresentedBackgroundImage { id, bytes });
                }
            });
        });
    });
    cx.global_mut::<BackgroundImageRuntime>().load = Some(load);
}
