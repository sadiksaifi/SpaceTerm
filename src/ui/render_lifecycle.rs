use crate::terminal::PresentationGeneration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SurfaceVisibility {
    pub(crate) application_active: bool,
    pub(crate) key_window: bool,
    pub(crate) minimized: bool,
    pub(crate) occluded: bool,
    pub(crate) live_resize: bool,
    pub(crate) workspace_visible: bool,
    pub(crate) pane_visible: bool,
}

impl SurfaceVisibility {
    fn presentable(self) -> bool {
        !self.minimized && !self.occluded && self.workspace_visible && self.pane_visible
    }

    fn animations_active(self) -> bool {
        self.presentable() && self.application_active && self.key_window && !self.live_resize
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RenderLifecycleEffects {
    pub(crate) request_redraw: bool,
    pub(crate) animations_active: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScaleChange {
    Unchanged,
    ScaleResources,
}

#[derive(Debug)]
pub(crate) struct RenderLifecycle {
    visibility: SurfaceVisibility,
    latest: Option<PresentationGeneration>,
    presented: Option<PresentationGeneration>,
    scale: Option<f32>,
    released: bool,
}

impl RenderLifecycle {
    pub(crate) fn new(visibility: SurfaceVisibility) -> Self {
        Self {
            visibility,
            latest: None,
            presented: None,
            scale: None,
            released: false,
        }
    }

    pub(crate) fn observe_snapshot(
        &mut self,
        generation: PresentationGeneration,
    ) -> RenderLifecycleEffects {
        if self.released {
            return self.effects();
        }
        if self.latest.is_none_or(|latest| generation >= latest) {
            self.latest = Some(generation);
        }
        self.effects_with_redraw(self.has_pending_frame())
    }

    pub(crate) fn update_visibility(
        &mut self,
        visibility: SurfaceVisibility,
    ) -> RenderLifecycleEffects {
        let was_presentable = self.visibility.presentable();
        self.visibility = visibility;
        let restored = !was_presentable && visibility.presentable();
        self.effects_with_redraw(restored && self.has_pending_frame())
    }

    pub(crate) fn update_product_visibility(
        &mut self,
        workspace_visible: bool,
        pane_visible: bool,
    ) -> RenderLifecycleEffects {
        let mut visibility = self.visibility;
        visibility.workspace_visible = workspace_visible;
        visibility.pane_visible = pane_visible;
        self.update_visibility(visibility)
    }

    pub(crate) fn effects(&self) -> RenderLifecycleEffects {
        self.effects_with_redraw(false)
    }

    pub(crate) fn take_frame(&self) -> Option<PresentationGeneration> {
        (self.visibility.presentable() && !self.released)
            .then_some(self.latest)
            .flatten()
            .filter(|latest| Some(*latest) != self.presented)
    }

    pub(crate) fn mark_presented(&mut self, generation: PresentationGeneration) {
        if !self.released && self.latest.is_some_and(|latest| generation <= latest) {
            self.presented = Some(generation);
        }
    }

    pub(crate) fn is_presented(&self, generation: PresentationGeneration) -> bool {
        !self.released && self.presented == Some(generation)
    }

    pub(crate) fn update_scale(&mut self, scale: f32) -> ScaleChange {
        if self.released || !scale.is_finite() || scale <= 0.0 || self.scale == Some(scale) {
            return ScaleChange::Unchanged;
        }
        self.scale = Some(scale);
        ScaleChange::ScaleResources
    }

    pub(crate) fn release(&mut self) {
        self.released = true;
        self.latest = None;
    }

    fn has_pending_frame(&self) -> bool {
        self.latest.is_some() && self.latest != self.presented
    }

    fn effects_with_redraw(&self, request_redraw: bool) -> RenderLifecycleEffects {
        RenderLifecycleEffects {
            request_redraw: request_redraw && self.visibility.presentable() && !self.released,
            animations_active: self.visibility.animations_active() && !self.released,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::PresentationGeneration;

    fn visible() -> SurfaceVisibility {
        SurfaceVisibility {
            application_active: true,
            key_window: true,
            minimized: false,
            occluded: false,
            live_resize: false,
            workspace_visible: true,
            pane_visible: true,
        }
    }

    #[test]
    fn hidden_surfaces_coalesce_to_latest_snapshot_and_restore_once() {
        let mut lifecycle = RenderLifecycle::new(SurfaceVisibility {
            occluded: true,
            ..visible()
        });
        assert!(
            !lifecycle
                .observe_snapshot(PresentationGeneration::test(1))
                .request_redraw
        );
        assert!(
            !lifecycle
                .observe_snapshot(PresentationGeneration::test(2))
                .request_redraw
        );

        let restored = lifecycle.update_visibility(visible());
        assert!(restored.request_redraw);
        assert_eq!(
            lifecycle.take_frame(),
            Some(PresentationGeneration::test(2))
        );
        lifecycle.mark_presented(PresentationGeneration::test(2));
        assert!(!lifecycle.update_visibility(visible()).request_redraw);
    }

    #[test]
    fn product_visibility_updates_unrendered_zoomed_panes() {
        let mut lifecycle = RenderLifecycle::new(visible());
        lifecycle.observe_snapshot(PresentationGeneration::test(1));
        lifecycle.mark_presented(PresentationGeneration::test(1));
        lifecycle.update_product_visibility(true, false);
        assert!(
            !lifecycle
                .observe_snapshot(PresentationGeneration::test(2))
                .request_redraw
        );
        assert!(lifecycle.take_frame().is_none());

        assert!(
            lifecycle
                .update_product_visibility(true, true)
                .request_redraw
        );
        assert_eq!(
            lifecycle.take_frame(),
            Some(PresentationGeneration::test(2))
        );
    }

    #[test]
    fn recurring_effects_require_every_animation_visibility_fact() {
        let mut lifecycle = RenderLifecycle::new(visible());
        assert!(lifecycle.effects().animations_active);
        for hidden in [
            SurfaceVisibility {
                application_active: false,
                ..visible()
            },
            SurfaceVisibility {
                key_window: false,
                ..visible()
            },
            SurfaceVisibility {
                minimized: true,
                ..visible()
            },
            SurfaceVisibility {
                occluded: true,
                ..visible()
            },
            SurfaceVisibility {
                live_resize: true,
                ..visible()
            },
            SurfaceVisibility {
                workspace_visible: false,
                ..visible()
            },
            SurfaceVisibility {
                pane_visible: false,
                ..visible()
            },
        ] {
            assert!(!lifecycle.update_visibility(hidden).animations_active);
        }
    }

    #[test]
    fn scale_changes_invalidate_only_scale_dependent_resources() {
        let mut lifecycle = RenderLifecycle::new(visible());
        assert_eq!(lifecycle.update_scale(2.0), ScaleChange::ScaleResources);
        assert_eq!(lifecycle.update_scale(2.0), ScaleChange::Unchanged);
        lifecycle.release();
        assert!(!lifecycle.effects().animations_active);
        assert_eq!(lifecycle.take_frame(), None);
    }
}
