#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TrailingSpacePolicy {
    Preserve,
    #[default]
    Trim,
}

impl TrailingSpacePolicy {
    pub const fn from_trim_enabled(enabled: bool) -> Self {
        if enabled { Self::Trim } else { Self::Preserve }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionCopyOptions {
    pub unwrap_soft_wraps: bool,
    pub trailing_spaces: TrailingSpacePolicy,
    pub include_html: bool,
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

#[derive(Clone, Eq, PartialEq)]
pub struct SelectionCopy {
    pub plain_text: String,
    pub html: Option<String>,
}

impl std::fmt::Debug for SelectionCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectionCopy").finish_non_exhaustive()
    }
}
