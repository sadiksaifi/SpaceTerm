//! Font families and private chrome faces supplied by desktop composition.

#[derive(Clone)]
pub(crate) struct HostFonts {
    pub(crate) ui_family: String,
    pub(crate) system_monospace_family: String,
    pub(crate) terminal_families: &'static [&'static str],
    pub(crate) emoji_family: String,
    pub(crate) bundled_ui_faces: &'static [&'static [u8]],
}

impl Default for HostFonts {
    fn default() -> Self {
        Self {
            ui_family: "system-ui".into(),
            system_monospace_family: "monospace".into(),
            terminal_families: &[crate::bundled_font::FAMILY],
            emoji_family: "emoji".into(),
            bundled_ui_faces: &[],
        }
    }
}

impl gpui::Global for HostFonts {}

impl HostFonts {
    pub(crate) fn get(cx: &gpui::App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_default()
    }
}
