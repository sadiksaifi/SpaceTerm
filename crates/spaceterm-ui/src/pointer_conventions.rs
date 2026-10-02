//! Desktop pointer gestures, explicitly selected alongside the keyboard profiles.

use gpui::{App, Global, Modifiers, MouseButton};

/// Whether Control modifies a primary click into a desktop secondary click.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PointerConventions {
    /// macOS accepts both the secondary button and Control with the primary button.
    #[default]
    ControlClickSecondary,
    /// Linux reserves secondary clicks for the secondary button.
    SecondaryButton,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_link_modifiers_keep_control_click_available_on_linux() {
        let control = Modifiers::control();
        let command = Modifiers {
            platform: true,
            ..Modifiers::none()
        };
        assert!(PointerConventions::SecondaryButton.activates_link(control));
        assert!(!PointerConventions::SecondaryButton.activates_link(command));
        assert!(PointerConventions::ControlClickSecondary.activates_link(command));
        assert!(!PointerConventions::ControlClickSecondary.activates_link(control));
        for policy in [
            PointerConventions::SecondaryButton,
            PointerConventions::ControlClickSecondary,
        ] {
            assert!(!policy.activates_link(Modifiers::none()));
            assert!(policy.secondary(MouseButton::Right, Modifiers::none()));
        }
    }
}

impl Global for PointerConventions {}

/// Installs the host's pointer policy without performing operating-system detection.
pub fn install_pointer_conventions(cx: &mut App, conventions: PointerConventions) {
    cx.set_global(conventions);
}

impl PointerConventions {
    /// The installed desktop policy. Standalone controls retain the macOS convention.
    pub fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }

    /// Whether the desktop's modifier for following a link is held.
    pub fn activates_link(self, modifiers: Modifiers) -> bool {
        match self {
            Self::ControlClickSecondary => modifiers.platform,
            Self::SecondaryButton => modifiers.control,
        }
    }

    pub(crate) fn secondary(self, button: MouseButton, modifiers: Modifiers) -> bool {
        button == MouseButton::Right
            || (self == Self::ControlClickSecondary
                && button == MouseButton::Left
                && modifiers.control
                && !modifiers.alt
                && !modifiers.platform)
    }

    pub(crate) fn primary(self, button: MouseButton, modifiers: Modifiers) -> bool {
        button == MouseButton::Left && (self == Self::SecondaryButton || !modifiers.control)
    }
}
