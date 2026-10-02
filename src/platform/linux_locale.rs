//! Resolve the desktop locale against SpaceTerm's shipped localizations.
use spaceterm_ui::TextDirection;
pub(super) struct LinuxLocale { direction: TextDirection }
impl LinuxLocale {
    pub(super) fn capture(read: impl Fn(&str) -> Option<String>) -> Self {
        let preferred = preferred_languages(read);
        Self { direction: super::locale::resolved_direction(&preferred, &["en"]) }
    }
}
fn preferred_languages(read: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let effective = ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter()
        .find_map(|key| read(key).filter(|value| !value.is_empty())).unwrap_or_else(|| "C".into());
    if matches!(effective.split('.').next(), Some("C" | "POSIX")) { return vec!["en".into()]; }
    let mut preferred: Vec<_> = read("LANGUAGE").into_iter().flat_map(|value| value.split(':').filter(|value| !value.is_empty()).map(str::to_owned).collect::<Vec<_>>()).collect();
    preferred.push(effective);
    preferred
}
impl super::locale::LocaleDirection for LinuxLocale { fn text_direction(&self) -> TextDirection { self.direction } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_preferences_respect_posix_precedence_and_c_locale() {
        let read = |key: &str| match key { "LANGUAGE" => Some("ar:en".into()), "LC_ALL" => Some("C.UTF-8".into()), "LANG" => Some("ar_EG.UTF-8".into()), _ => None };
        assert_eq!(preferred_languages(read), ["en"]);
        let read = |key: &str| match key { "LANGUAGE" => Some("ar:en".into()), "LC_MESSAGES" => Some("fr_FR.UTF-8".into()), "LANG" => Some("en_US".into()), _ => None };
        assert_eq!(preferred_languages(read), ["ar", "en", "fr_FR.UTF-8"]);
    }
}
