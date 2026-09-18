use std::ops::Range;

use fuzzy_matcher::FuzzyMatcher as _;
use fuzzy_matcher::skim::SkimMatcherV2;
use unicode_casefold::UnicodeCaseFold as _;

/// The searchable text for one fuzzy-filter candidate.
///
/// Fields are joined in insertion order. The first field is normally the visible label, followed
/// by any visible description and non-presentational keywords.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FuzzyTarget {
    text: String,
    fields: Vec<Range<usize>>,
}

impl FuzzyTarget {
    /// Starts a target with its primary visible field.
    pub fn new(value: impl AsRef<str>) -> Self {
        Self::default().field(value)
    }

    /// Appends one searchable field and records its character-index range.
    pub fn field(mut self, value: impl AsRef<str>) -> Self {
        let value = value.as_ref();
        let start = self.text.chars().count() + usize::from(!self.fields.is_empty());
        if !self.fields.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(value);
        self.fields.push(start..start + value.chars().count());
        self
    }
}

/// One ranked fuzzy-filter result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FuzzyMatch {
    item_index: usize,
    score: i64,
    highlight_indices: Vec<usize>,
    fields: Vec<Range<usize>>,
}

impl FuzzyMatch {
    /// Returns the candidate's index in the source slice.
    pub const fn item_index(&self) -> usize {
        self.item_index
    }

    /// Returns the ranking score derived from the matcher score and consecutive-character bonus.
    pub const fn score(&self) -> i64 {
        self.score
    }

    /// Returns matched character indices in the complete joined target.
    pub fn highlight_indices(&self) -> &[usize] {
        &self.highlight_indices
    }

    /// Returns matched character indices relative to one target field.
    pub fn field_highlight_indices(&self, field: usize) -> Vec<usize> {
        let Some(range) = self.fields.get(field) else {
            return Vec::new();
        };
        self.highlight_indices
            .iter()
            .filter_map(|index| {
                if range.contains(index) {
                    Some(index - range.start)
                } else {
                    None
                }
            })
            .collect()
    }
}

/// Filters and ranks candidates by the target each caller supplies.
///
/// Matching is Unicode case-insensitive. An empty or whitespace-only query returns every item in
/// source order with no highlights.
pub fn fuzzy_filter<T>(
    items: &[T],
    query: &str,
    target: impl Fn(&T) -> FuzzyTarget,
) -> Vec<FuzzyMatch> {
    let query = query.trim();
    if query.is_empty() {
        return items
            .iter()
            .enumerate()
            .map(|(item_index, _)| FuzzyMatch {
                item_index,
                score: 0,
                highlight_indices: Vec::new(),
                fields: Vec::new(),
            })
            .collect();
    }

    let normalized_query = fold_case(query).0;
    let matcher = SkimMatcherV2::default();
    let mut matches = items
        .iter()
        .enumerate()
        .filter_map(|(item_index, item)| {
            let target = target(item);
            let (normalized_target, source_indices) = fold_case(&target.text);
            let (score, normalized_indices) =
                matcher.fuzzy_indices(&normalized_target, &normalized_query)?;
            let mut highlight_indices = normalized_indices
                .into_iter()
                .filter_map(|index| source_indices.get(index).copied())
                .collect::<Vec<_>>();
            highlight_indices.dedup();
            let contiguous_pairs = highlight_indices
                .windows(2)
                .filter(|pair| pair[1] == pair[0] + 1)
                .count() as i64;
            Some(FuzzyMatch {
                item_index,
                score: score + contiguous_pairs * 20,
                highlight_indices,
                fields: target.fields,
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.item_index.cmp(&right.item_index))
    });
    matches
}

/// Converts character highlight indices into maximal byte ranges safe for UTF-8 slicing.
pub fn highlight_ranges(text: &str, indices: &[usize]) -> Vec<Range<usize>> {
    let mut indices = indices.to_vec();
    indices.sort_unstable();
    indices.dedup();
    let boundaries = text
        .char_indices()
        .map(|(start, character)| start..start + character.len_utf8())
        .collect::<Vec<_>>();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for index in indices {
        let Some(next) = boundaries.get(index).cloned() else {
            continue;
        };
        if let Some(previous) = ranges.last_mut()
            && previous.end == next.start
        {
            previous.end = next.end;
        } else {
            ranges.push(next);
        }
    }
    ranges
}

fn fold_case(text: &str) -> (String, Vec<usize>) {
    let mut normalized = String::new();
    let mut source_indices = Vec::new();
    for (source_index, character) in text.chars().enumerate() {
        for normalized_character in character.case_fold() {
            normalized.push(normalized_character);
            source_indices.push(source_index);
        }
    }
    (normalized, source_indices)
}

#[cfg(test)]
mod tests {
    use super::{FuzzyTarget, fuzzy_filter, highlight_ranges};

    #[test]
    fn empty_query_preserves_source_order() {
        let items = ["third", "first", "second"];

        let matches = fuzzy_filter(&items, "  ", |item| FuzzyTarget::new(item));

        assert_eq!(
            matches
                .iter()
                .map(super::FuzzyMatch::item_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn contiguous_match_ranks_above_non_contiguous_match() {
        let items = ["remote operation", "projects"];

        let matches = fuzzy_filter(&items, "ro", |item| FuzzyTarget::new(item));

        assert_eq!(matches[0].item_index(), 1, "matches: {matches:?}");
    }

    #[test]
    fn unicode_case_insensitive_match_keeps_source_character_indices() {
        let items = ["Ångström"];

        let matches = fuzzy_filter(&items, "ång", |item| FuzzyTarget::new(item));

        assert_eq!(matches[0].field_highlight_indices(0), vec![0, 1, 2]);
    }

    #[test]
    fn unicode_case_folding_matches_contextual_sigma() {
        let items = ["ΟΣ"];

        let matches = fuzzy_filter(&items, "ος", |item| FuzzyTarget::new(item));

        assert_eq!(matches[0].field_highlight_indices(0), vec![0, 1]);
    }

    #[test]
    fn unicode_case_folding_matches_expanding_characters() {
        let items = ["Straße"];

        let matches = fuzzy_filter(&items, "STRASSE", |item| FuzzyTarget::new(item));

        assert_eq!(
            matches[0].field_highlight_indices(0),
            vec![0, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn target_fields_report_relative_highlight_indices() {
        let items = [("Open", "Choose a directory", ["folder"])];

        let matches = fuzzy_filter(&items, "directory", |item| {
            FuzzyTarget::new(item.0).field(item.1).field(item.2[0])
        });

        assert_eq!(
            matches[0].field_highlight_indices(1),
            (9..18).collect::<Vec<_>>()
        );
    }

    #[test]
    fn highlight_ranges_are_utf8_safe_and_merge_adjacent_characters() {
        assert_eq!(highlight_ranges("Éclair 🔍", &[0, 6, 7]), vec![0..2, 7..12]);
    }
}
