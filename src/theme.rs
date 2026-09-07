//! Resolves Zed theme data into application-owned semantic colors.
//!
//! The bundled default is generated from the pinned Vague Pro submodule at build time.
//! Rendering never reads files or parses JSON. Theme installation and selection are separate UI.

mod color;
pub(crate) use color::Color;
use serde_json::{Map, Value};
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Appearance {
    Light,
    Dark,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlayerTheme {
    pub(crate) background: Color,
    pub(crate) cursor: Color,
    pub(crate) selection: Color,
}

macro_rules! theme_colors {
    ($($field:ident => $key:literal,)*) => {
        #[derive(Clone, Debug)]
        pub(crate) struct Theme {
            pub(crate) name: String,
            pub(crate) appearance: Appearance,
            pub(crate) accents: Vec<Color>,
            pub(crate) players: Vec<PlayerTheme>,
            pub(crate) syntax: Value,
            pub(crate) modal_scrim: Color,
            $(pub(crate) $field: Color,)*
        }

        impl Theme {
            fn apply_style(&mut self, style: &Map<String, Value>) -> Result<(), ThemeError> {
                $(if let Some(color) = optional_color(style.get($key))? {
                    self.$field = color;
                })*
                // Zed has no scrim token. This application-owned overlay follows its background.
                self.modal_scrim = Color { a: 0x99, ..self.background };
                Ok(())
            }
        }
    }
}
include!("theme/tokens.rs");

include!(concat!(env!("OUT_DIR"), "/default_theme.rs"));
pub(crate) use VAGUE_PRO as ACTIVE_THEME;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThemeError {
    InvalidDocument,
    InvalidColor,
    UnknownTheme,
    AppearanceMismatch,
}

fn optional_color(value: Option<&Value>) -> Result<Option<Color>, ThemeError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Color::parse(value)
            .map(Some)
            .map_err(|_| ThemeError::InvalidColor),
        _ => Err(ThemeError::InvalidColor),
    }
}

impl Theme {
    /// Resolves a named Zed theme over an explicit base of the same appearance.
    /// Missing and null tokens inherit; unknown roles remain harmless. Errors exclude input data.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "theme installation will consume this adapter")
    )]
    pub(crate) fn from_zed_json(json: &str, name: &str, base: &Self) -> Result<Self, ThemeError> {
        if json.len() > 4 * 1024 * 1024 {
            return Err(ThemeError::InvalidDocument);
        }
        let family: Value = serde_json::from_str(json).map_err(|_| ThemeError::InvalidDocument)?;
        let themes = family
            .get("themes")
            .and_then(Value::as_array)
            .ok_or(ThemeError::InvalidDocument)?;
        let content = themes
            .iter()
            .find(|theme| theme.get("name").and_then(Value::as_str) == Some(name))
            .ok_or(ThemeError::UnknownTheme)?;
        let appearance = match content.get("appearance").and_then(Value::as_str) {
            Some("light") => Appearance::Light,
            Some("dark") => Appearance::Dark,
            _ => return Err(ThemeError::InvalidDocument),
        };
        if appearance != base.appearance {
            return Err(ThemeError::AppearanceMismatch);
        }
        let style = content
            .get("style")
            .and_then(Value::as_object)
            .ok_or(ThemeError::InvalidDocument)?;
        let mut theme = base.clone();
        theme.name = name.to_owned();
        theme.apply_style(style)?;
        if let Some(value) = style.get("accents") {
            let values = value.as_array().ok_or(ThemeError::InvalidDocument)?;
            if !values.is_empty() {
                theme.accents = values
                    .iter()
                    .map(|value| {
                        optional_color(Some(value)).map(|color| color.unwrap_or(theme.text_accent))
                    })
                    .collect::<Result<_, _>>()?;
            }
        }
        if let Some(value) = style.get("players") {
            let values = value.as_array().ok_or(ThemeError::InvalidDocument)?;
            if !values.is_empty() {
                let base_selection = base
                    .players
                    .first()
                    .ok_or(ThemeError::InvalidDocument)?
                    .selection;
                theme.players = values
                    .iter()
                    .map(|value| {
                        let player = value.as_object().ok_or(ThemeError::InvalidDocument)?;
                        Ok(PlayerTheme {
                            background: optional_color(player.get("background"))?
                                .unwrap_or(theme.text),
                            cursor: optional_color(player.get("cursor"))?.unwrap_or(theme.text),
                            selection: optional_color(player.get("selection"))?
                                .unwrap_or(base_selection),
                        })
                    })
                    .collect::<Result<_, ThemeError>>()?;
            }
        }
        if let Some(value) = style.get("syntax") {
            let syntax = value.as_object().ok_or(ThemeError::InvalidDocument)?;
            if let Some(resolved) = theme.syntax.as_object_mut() {
                for (name, style) in syntax {
                    resolved.insert(name.clone(), style.clone());
                }
            }
        }
        Ok(theme)
    }
}

impl Theme {
    pub(crate) const fn terminal_normal(&self) -> [Color; 8] {
        [
            self.terminal_ansi_black,
            self.terminal_ansi_red,
            self.terminal_ansi_green,
            self.terminal_ansi_yellow,
            self.terminal_ansi_blue,
            self.terminal_ansi_magenta,
            self.terminal_ansi_cyan,
            self.terminal_ansi_white,
        ]
    }

    pub(crate) const fn terminal_bright(&self) -> [Color; 8] {
        [
            self.terminal_ansi_bright_black,
            self.terminal_ansi_bright_red,
            self.terminal_ansi_bright_green,
            self.terminal_ansi_bright_yellow,
            self.terminal_ansi_bright_blue,
            self.terminal_ansi_bright_magenta,
            self.terminal_ansi_bright_cyan,
            self.terminal_ansi_bright_white,
        ]
    }

    pub(crate) const fn terminal_dim(&self) -> [Color; 8] {
        [
            self.terminal_ansi_dim_black,
            self.terminal_ansi_dim_red,
            self.terminal_ansi_dim_green,
            self.terminal_ansi_dim_yellow,
            self.terminal_ansi_dim_blue,
            self.terminal_ansi_dim_magenta,
            self.terminal_ansi_dim_cyan,
            self.terminal_ansi_dim_white,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const UPSTREAM: &str = include_str!("../third_party/vague-pro-zed/themes/vague-pro.json");

    #[test]
    fn bundled_theme_matches_the_zed_adapter_for_every_color() {
        let parsed = Theme::from_zed_json(UPSTREAM, "Vague Pro", &VAGUE_PRO).unwrap();
        let source: Value = serde_json::from_str(UPSTREAM).unwrap();
        let mut unmapped = source["themes"][0]["style"].as_object().unwrap().clone();
        macro_rules! theme_colors { ($($field:ident => $key:literal,)*) => {{
            $(assert_eq!(parsed.$field, VAGUE_PRO.$field, "{}", $key); unmapped.remove($key);)*
        }}; }
        include!("theme/tokens.rs");
        for key in ["accents", "players", "syntax"] {
            unmapped.remove(key);
        }
        assert!(
            unmapped.is_empty(),
            "new bundled theme roles need an explicit mapping"
        );
        assert_eq!(parsed.accents, VAGUE_PRO.accents);
        assert_eq!(parsed.players, VAGUE_PRO.players);
        assert_eq!(parsed.syntax, VAGUE_PRO.syntax);
    }

    #[test]
    fn partial_theme_preserves_alpha_and_inherits_null_or_missing_roles() {
        let theme = Theme::from_zed_json(r##"{"themes":[{"name":"Custom","appearance":"dark","style":{"text":"#abc8","icon":null,"future.role":"anything"}}]}"##, "Custom", &VAGUE_PRO).unwrap();
        assert_eq!(theme.text, Color::rgba(0xaabbcc88));
        assert_eq!(theme.icon, VAGUE_PRO.icon);
        assert_eq!(theme.terminal_background, VAGUE_PRO.terminal_background);
    }

    #[test]
    fn malformed_color_is_a_content_free_failure_and_does_not_modify_the_base() {
        let result = Theme::from_zed_json(
            r##"{"themes":[{"name":"Invalid","appearance":"dark","style":{"text":"private path"}}]}"##,
            "Invalid",
            &VAGUE_PRO,
        );
        assert_eq!(result.unwrap_err(), ThemeError::InvalidColor);
        assert_eq!(VAGUE_PRO.text, Color::rgb(0xcdcdcd));
    }

    #[test]
    fn appearance_requires_an_explicit_matching_base() {
        let result = Theme::from_zed_json(
            r#"{"themes":[{"name":"Light","appearance":"light","style":{}}]}"#,
            "Light",
            &VAGUE_PRO,
        );
        assert_eq!(result.unwrap_err(), ThemeError::AppearanceMismatch);
    }
}
