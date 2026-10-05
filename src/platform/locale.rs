use spaceterm_ui::TextDirection;
/// Application locale facts sampled after GPUI has initialized the native application.
pub(crate) trait LocaleDirection {
    fn text_direction(&self) -> TextDirection;
}
#[cfg(test)]
pub(crate) struct FixedLocaleDirection(pub(crate) TextDirection);
#[cfg(test)]
impl LocaleDirection for FixedLocaleDirection {
    fn text_direction(&self) -> TextDirection {
        self.0
    }
}

/// Direction belongs to the resolved application localization, not an unsupported host language.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn resolved_direction(preferred: &[String], supported: &[&str]) -> TextDirection {
    fn language(value: &str) -> String {
        value
            .split(['_', '-', '.', '@'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
    let resolved = preferred
        .iter()
        .map(|value| language(value))
        .find(|candidate| supported.iter().any(|value| language(value) == *candidate))
        .unwrap_or_else(|| "en".into());
    if [
        "ar", "he", "fa", "ur", "yi", "ps", "dv", "ckb", "sd", "ug", "syr",
    ]
    .contains(&resolved.as_str())
    {
        TextDirection::RightToLeft
    } else {
        TextDirection::LeftToRight
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_host_languages_do_not_reverse_the_english_ui() {
        assert_eq!(
            resolved_direction(&["ar_EG.UTF-8".into()], &["en"]),
            TextDirection::LeftToRight
        );
        assert_eq!(
            resolved_direction(&["ar_EG.UTF-8".into()], &["en", "ar"]),
            TextDirection::RightToLeft
        );
        assert_eq!(
            resolved_direction(&["fr".into(), "he-IL".into()], &["en", "he"]),
            TextDirection::RightToLeft
        );
    }
}
