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
