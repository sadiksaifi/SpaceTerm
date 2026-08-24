use gpui::prelude::*;
use gpui::{App, IntoElement, RenderOnce, SharedString, Window, div};

#[derive(IntoElement)]
pub struct MiddleTruncatedText {
    text: SharedString,
    maximum_characters: usize,
}

impl MiddleTruncatedText {
    pub fn new(text: impl Into<SharedString>, maximum_characters: usize) -> Self {
        Self {
            text: text.into(),
            maximum_characters,
        }
    }

    pub fn truncated_text(&self) -> SharedString {
        middle_truncate(&self.text, self.maximum_characters).into()
    }
}

impl RenderOnce for MiddleTruncatedText {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .w_full()
            .overflow_hidden()
            .child(self.truncated_text())
    }
}

fn middle_truncate(text: &str, maximum_characters: usize) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() <= maximum_characters {
        return text.to_owned();
    }
    if maximum_characters <= 1 {
        return "…".to_owned();
    }
    let visible = maximum_characters - 1;
    let prefix = visible.div_ceil(2);
    let suffix = visible / 2;
    characters[..prefix]
        .iter()
        .chain(std::iter::once(&'…'))
        .chain(characters[characters.len() - suffix..].iter())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_both_ends_when_text_exceeds_the_limit() {
        assert_eq!(middle_truncate("~/projects/SpaceTerm", 12), "~/proj…eTerm");
    }

    #[test]
    fn preserves_short_text() {
        assert_eq!(middle_truncate("~/src", 12), "~/src");
    }
}
