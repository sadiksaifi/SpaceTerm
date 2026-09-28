use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, KeyBindingMetaIndex};
use thiserror::Error;

use super::{Command, KeybindingPreferences, Shortcut, ShortcutRejection, TerminalConvention};

pub const CUSTOMIZABLE_BINDINGS: KeyBindingMetaIndex = KeyBindingMetaIndex(0x5354_4b42);

/// Host spelling of a primary shortcut and any aliases, validated by the profile.
#[derive(Clone, Debug)]
pub struct DefaultBinding {
    pub primary: String,
    pub aliases: Vec<String>,
}

impl DefaultBinding {
    pub fn new(primary: &str, aliases: &[&str]) -> Self {
        Self {
            primary: primary.into(),
            aliases: aliases.iter().map(|s| (*s).into()).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemReservation {
    Copy,
    Paste,
    Cut,
    Undo,
    Redo,
    SelectAll,
    Quit,
    Hide,
    HideOthers,
    Minimize,
    MinimizeAll,
    FullScreen,
    AppSwitcher,
    WindowCycling,
    Spotlight,
    CharacterViewer,
    ForceQuit,
    LockScreen,
    LogOut,
    Screenshot,
    Help,
    Settings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemReserved {
    pub shortcut: Shortcut,
    pub reason: SystemReservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum Reservation {
    #[error("shortcut is reserved for terminal input")]
    Terminal(TerminalConvention),
    #[error("shortcut is reserved by the operating system")]
    System(SystemReservation),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum KeymapProfileError {
    #[error("command has multiple default entries")]
    DuplicateCommand,
    #[error("invalid default shortcut")]
    InvalidDefault(ShortcutRejection),
    #[error("default shortcuts must be unique")]
    DuplicateShortcut,
    #[error("default shortcut is reserved by the operating system")]
    SystemReserved(SystemReservation),
    #[error("system reservation has multiple entries")]
    DuplicateSystemReservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeybindingState {
    Default,
    Overridden,
    Unassigned,
    Displaced { by: Command },
    Blocked(SystemReservation),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Reassignment {
    pub displaced: Option<Command>,
}

#[derive(Clone, Debug)]
pub struct KeymapProfile {
    defaults: BTreeMap<Command, Vec<Shortcut>>,
    system_reserved: HashMap<Shortcut, SystemReservation>,
    fixed_bindings: Vec<KeyBinding>,
    control_bindings: Vec<KeyBinding>,
}

impl KeymapProfile {
    /// Omitted commands have no default. An explicit `None` has the same meaning.
    pub fn new(
        defaults: impl IntoIterator<Item = (Command, Option<DefaultBinding>)>,
        system_reserved: Vec<SystemReserved>,
        fixed_bindings: Vec<KeyBinding>,
        control_bindings: Vec<KeyBinding>,
    ) -> Result<Self, KeymapProfileError> {
        let mut reservations = HashMap::new();
        for reserved in system_reserved {
            if reservations
                .insert(reserved.shortcut, reserved.reason)
                .is_some()
            {
                return Err(KeymapProfileError::DuplicateSystemReservation);
            }
        }
        let mut parsed_defaults = BTreeMap::new();
        let mut seen = HashSet::new();
        for (command, default) in defaults {
            if parsed_defaults.contains_key(&command) {
                return Err(KeymapProfileError::DuplicateCommand);
            }
            let mut shortcuts = Vec::new();
            if let Some(default) = default {
                for source in std::iter::once(default.primary).chain(default.aliases) {
                    let shortcut =
                        Shortcut::parse(&source).map_err(KeymapProfileError::InvalidDefault)?;
                    if let Some(&reason) = reservations.get(&shortcut) {
                        return Err(KeymapProfileError::SystemReserved(reason));
                    }
                    if !seen.insert(shortcut.clone()) {
                        return Err(KeymapProfileError::DuplicateShortcut);
                    }
                    shortcuts.push(shortcut);
                }
            }
            parsed_defaults.insert(command, shortcuts);
        }
        Ok(Self {
            defaults: parsed_defaults,
            system_reserved: reservations,
            fixed_bindings,
            control_bindings,
        })
    }

    pub fn check(&self, shortcut: &Shortcut) -> Result<(), Reservation> {
        if let Some(convention) = super::terminal_conventions::reservation(shortcut) {
            return Err(Reservation::Terminal(convention));
        }
        if let Some(&reason) = self.system_reserved.get(shortcut) {
            return Err(Reservation::System(reason));
        }
        Ok(())
    }

    pub fn resolve(&self, preferences: &KeybindingPreferences) -> ResolvedKeymap {
        let mut resolved = ResolvedKeymap::default();
        for (command, shortcut) in preferences.iter() {
            let (shortcuts, state) = match shortcut {
                None => (Vec::new(), KeybindingState::Unassigned),
                Some(shortcut) => {
                    if let Some(&reason) = self.system_reserved.get(shortcut) {
                        (Vec::new(), KeybindingState::Blocked(reason))
                    } else if let Some(by) = resolved.owner(shortcut) {
                        // Invalid duplicate overrides still cannot install two owners.
                        (Vec::new(), KeybindingState::Displaced { by })
                    } else {
                        resolved.owners.insert(shortcut.clone(), command);
                        (vec![shortcut.clone()], KeybindingState::Overridden)
                    }
                }
            };
            resolved
                .commands
                .insert(command, ResolvedCommand { shortcuts, state });
        }
        for command in Command::ALL {
            if preferences.is_overridden(command) {
                continue;
            }
            let defaults = self.defaults(command);
            let Some(primary) = defaults.first() else {
                resolved.commands.insert(
                    command,
                    ResolvedCommand {
                        shortcuts: Vec::new(),
                        state: KeybindingState::Unassigned,
                    },
                );
                continue;
            };
            if let Some(by) = resolved.owner(primary) {
                resolved.commands.insert(
                    command,
                    ResolvedCommand {
                        shortcuts: Vec::new(),
                        state: KeybindingState::Displaced { by },
                    },
                );
                continue;
            }
            let mut shortcuts = Vec::new();
            for shortcut in defaults {
                if resolved.owner(shortcut).is_none() {
                    resolved.owners.insert(shortcut.clone(), command);
                    shortcuts.push(shortcut.clone());
                }
            }
            resolved.commands.insert(
                command,
                ResolvedCommand {
                    shortcuts,
                    state: KeybindingState::Default,
                },
            );
        }
        resolved
    }

    pub fn assign(
        &self,
        preferences: &mut KeybindingPreferences,
        command: Command,
        shortcut: Option<Shortcut>,
    ) -> Result<Reassignment, Reservation> {
        if let Some(shortcut) = &shortcut {
            self.check(shortcut)?;
        }
        let displaced = shortcut
            .as_ref()
            .and_then(|shortcut| self.displace(preferences, command, shortcut));
        self.retain(preferences, command, shortcut);
        Ok(Reassignment { displaced })
    }

    /// Restore the primary and aliases, reclaiming each from its current owner.
    pub fn reset(&self, preferences: &mut KeybindingPreferences, command: Command) {
        for shortcut in self.defaults(command) {
            self.displace(preferences, command, shortcut);
        }
        preferences.remove(command);
    }

    fn displace(
        &self,
        preferences: &mut KeybindingPreferences,
        command: Command,
        shortcut: &Shortcut,
    ) -> Option<Command> {
        let resolved = self.resolve(preferences);
        let owner = resolved.owner(shortcut).filter(|&owner| owner != command)?;
        let primary = resolved.shortcut(owner);
        let kept = primary.filter(|&primary| primary != shortcut).cloned();
        self.retain(preferences, owner, kept);
        Some(owner)
    }

    /// Records one Keybinding. An override equal to an alias-free default, or Unassigned for a
    /// command without one, restates the default and is not retained.
    fn retain(
        &self,
        preferences: &mut KeybindingPreferences,
        command: Command,
        shortcut: Option<Shortcut>,
    ) {
        let restates_default = match (self.defaults(command), &shortcut) {
            ([], None) => true,
            ([default], Some(shortcut)) => default == shortcut,
            _ => false,
        };
        if restates_default {
            preferences.remove(command);
        } else {
            preferences.set(command, shortcut);
        }
    }

    fn defaults(&self, command: Command) -> &[Shortcut] {
        self.defaults.get(&command).map_or(&[], Vec::as_slice)
    }

    pub fn fixed_bindings(&self) -> &[KeyBinding] {
        &self.fixed_bindings
    }
    pub fn control_bindings(&self) -> &[KeyBinding] {
        &self.control_bindings
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedCommand {
    shortcuts: Vec<Shortcut>,
    state: KeybindingState,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedKeymap {
    commands: BTreeMap<Command, ResolvedCommand>,
    owners: HashMap<Shortcut, Command>,
}

impl ResolvedKeymap {
    pub fn shortcut(&self, command: Command) -> Option<&Shortcut> {
        self.shortcuts(command).first()
    }

    pub fn shortcuts(&self, command: Command) -> &[Shortcut] {
        self.commands
            .get(&command)
            .map_or(&[], |entry| entry.shortcuts.as_slice())
    }

    pub fn state(&self, command: Command) -> KeybindingState {
        self.commands
            .get(&command)
            .map_or(KeybindingState::Unassigned, |entry| entry.state)
    }

    pub fn owner(&self, shortcut: &Shortcut) -> Option<Command> {
        self.owners.get(shortcut).copied()
    }

    pub fn key_bindings(&self) -> Vec<KeyBinding> {
        let mut bindings = Vec::new();
        for command in Command::ALL {
            for shortcut in self.shortcuts(command) {
                bindings.push(binding(command, shortcut, command.key_context()));
                if command.mirrors_into_find_field() {
                    bindings.push(binding(
                        command,
                        shortcut,
                        Some(crate::ui::TERMINAL_FIND_KEY_CONTEXT),
                    ));
                }
            }
        }
        bindings
    }
}

fn binding(command: Command, shortcut: &Shortcut, context: Option<&str>) -> KeyBinding {
    let predicate = context.map(|context| {
        KeyBindingContextPredicate::parse(context)
            .expect("static command key context")
            .into()
    });
    KeyBinding::load(
        &shortcut.to_string(),
        command.action(),
        predicate,
        false,
        None,
        &DummyKeyboardMapper,
    )
    .expect("Shortcut guarantees GPUI-parseable spelling")
    .with_meta(CUSTOMIZABLE_BINDINGS)
}
