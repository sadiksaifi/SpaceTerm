pub(crate) use spaceterm_terminal::emulator::*;

impl spaceterm_terminal::appearance::TerminalAppearance
    for crate::appearance::ResolvedTerminalAppearance
{
    fn terminal_appearance(&self) -> crate::appearance::Appearance {
        self.appearance
    }
    fn terminal_colors(&self) -> &crate::appearance::TerminalColors {
        &self.colors
    }
    fn terminal_bold_as_bright(&self) -> bool {
        self.bold_as_bright
    }
}

impl spaceterm_terminal::appearance::AppearanceUpdate
    for crate::terminal::TerminalAppearanceUpdate
{
    fn terminal_update(&self) -> spaceterm_terminal::TerminalAppearanceUpdate {
        spaceterm_terminal::TerminalAppearanceUpdate::new(
            self.generation,
            std::sync::Arc::new(spaceterm_terminal::appearance::ResolvedTerminalAppearance {
                appearance: self.appearance.appearance,
                colors: self.appearance.colors.clone(),
                bold_as_bright: self.appearance.bold_as_bright,
            }),
        )
    }
}

impl From<crate::terminal::TerminalAppearanceUpdate>
    for spaceterm_terminal::TerminalAppearanceUpdate
{
    fn from(update: crate::terminal::TerminalAppearanceUpdate) -> Self {
        spaceterm_terminal::appearance::AppearanceUpdate::terminal_update(&update)
    }
}

#[cfg(test)]
#[path = "emulator/tests.rs"]
mod tests;
