use std::sync::Arc;

/// The longest Terminal Find query.
#[cfg(feature = "desktop")]
pub(crate) const MAX_QUERY_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "desktop")]
pub enum FindDirection {
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct FindQueryGeneration(u64);

impl FindQueryGeneration {
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[cfg(test)]
    pub const fn test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FindHighlightSpan {
    pub row: u16,
    pub start_column: u16,
    pub end_column: u16,
    pub current: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalFindSnapshot {
    pub generation: FindQueryGeneration,
    pub total_matches: usize,
    pub current_match: Option<usize>,
    pub visible_spans: Arc<[FindHighlightSpan]>,
}
