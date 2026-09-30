//! The logical text direction of the POSIX locale SpaceTerm was started with.
use spaceterm_ui::TextDirection;

/// Languages whose scripts are written right to left.
const RIGHT_TO_LEFT_LANGUAGES: &[&str] = &[
    "ar", "arc", "ckb", "dv", "fa", "ha", "he", "iw", "ks", "ku", "ps", "sd", "ug", "ur", "yi",
];

/// Captured once at startup from `LC_ALL`, `LC_MESSAGES`, then `LANG`, as POSIX resolves them.
pub(super) struct LinuxLocale {
    direction: TextDirection,
}

impl LinuxLocale {
    pub(super) fn capture(read: impl Fn(&str) -> Option<String>) -> Self {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|key| read(key).filter(|value| !value.is_empty()));
        Self {
            direction: locale.map_or(TextDirection::LeftToRight, |locale| direction(&locale)),
        }
    }
}

impl super::locale::LocaleDirection for LinuxLocale {
    fn text_direction(&self) -> TextDirection {
        self.direction
    }
}

fn direction(locale: &str) -> TextDirection {
    let language = locale
        .split(['_', '.', '@', '-'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if RIGHT_TO_LEFT_LANGUAGES.contains(&language.as_str()) {
        TextDirection::RightToLeft
    } else {
        TextDirection::LeftToRight
    }
}

#[cfg(test)]
mod tests {
    use super::super::locale::LocaleDirection;
    use super::*;

    fn capture(values: &[(&str, &str)]) -> TextDirection {
        LinuxLocale::capture(|key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        })
        .text_direction()
    }

    #[test]
    fn linux_locale_direction_follows_posix_precedence() {
        assert_eq!(capture(&[]), TextDirection::LeftToRight);
        assert_eq!(capture(&[("LANG", "he_IL.UTF-8")]), TextDirection::RightToLeft);
        assert_eq!(
            capture(&[("LANG", "he_IL.UTF-8"), ("LC_ALL", "en_US.UTF-8")]),
            TextDirection::LeftToRight
        );
        assert_eq!(
            capture(&[("LANG", "en_US.UTF-8"), ("LC_MESSAGES", "ar_EG")]),
            TextDirection::RightToLeft
        );
        assert_eq!(
            capture(&[("LC_ALL", ""), ("LANG", "fa_IR@persian")]),
            TextDirection::RightToLeft
        );
        assert_eq!(capture(&[("LANG", "C")]), TextDirection::LeftToRight);
    }
}
