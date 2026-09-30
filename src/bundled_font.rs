//! The bundled terminal family's private identity, faces, and native metadata.

pub(crate) const FAMILY: &str = "SpaceTerm Default";
pub(crate) const LABEL: &str = "Default (JetBrainsMono Nerd Font)";

pub(crate) const FACES: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMonoNerdFont-Regular.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMonoNerdFont-Bold.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMonoNerdFont-Italic.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMonoNerdFont-BoldItalic.ttf"),
];

/// Return PostScript and display names, matching the renderer's CSS weight search.
#[cfg(any(not(test), feature = "native-tests"))]
pub(crate) fn face_metadata(weight: u16, italic: bool) -> (&'static str, &'static str) {
    match (weight > 500, italic) {
        (false, false) => ("SpaceTermDefault-Regular", "SpaceTerm Default Regular"),
        (true, false) => ("SpaceTermDefault-Bold", "SpaceTerm Default Bold"),
        (false, true) => ("SpaceTermDefault-Italic", "SpaceTerm Default Italic"),
        (true, true) => (
            "SpaceTermDefault-BoldItalic",
            "SpaceTerm Default Bold Italic",
        ),
    }
}
