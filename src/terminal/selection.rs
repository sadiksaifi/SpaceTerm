#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TrailingSpacePolicy {
    Preserve,
    #[default]
    Trim,
}

impl TrailingSpacePolicy {
    pub(crate) const fn from_trim_enabled(enabled: bool) -> Self {
        if enabled { Self::Trim } else { Self::Preserve }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SelectionCopyOptions {
    pub(crate) unwrap_soft_wraps: bool,
    pub(crate) trailing_spaces: TrailingSpacePolicy,
    pub(crate) include_html: bool,
}

impl Default for SelectionCopyOptions {
    fn default() -> Self {
        Self {
            unwrap_soft_wraps: true,
            trailing_spaces: TrailingSpacePolicy::from_trim_enabled(true),
            include_html: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectionCopy {
    pub(crate) plain_text: String,
    pub(crate) html: Option<String>,
}
