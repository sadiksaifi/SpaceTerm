//! The bundled terminal family's private identity, faces, and native metadata.

pub(crate) const FAMILY: &str = "SpaceTerm Default";
pub(crate) const LABEL: &str = "Default (JetBrainsMono Nerd Font)";

pub(crate) const FACES: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMonoNerdFont-Regular.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMonoNerdFont-Bold.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMonoNerdFont-Italic.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMonoNerdFont-BoldItalic.ttf"),
];
