//! The release notes this build carries for its own version.
//!
//! The release pipeline names a git-cliff notes file through `SPACETERM_RELEASE_NOTES`, and
//! build.rs embeds it (`build/release_notes.rs`). A build without that file carries no notes, and
//! nothing offers "What's new".

use std::sync::LazyLock;

const VERSION: &str = env!("SPACETERM_VERSION");
const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/release-notes.md"));
const RELEASED: &str = env!("SPACETERM_RELEASE_NOTES_DATE");
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

static NOTES: LazyLock<Option<ReleaseNotes>> =
    LazyLock::new(|| ReleaseNotes::parse(VERSION, RELEASED, EMBEDDED));

/// The notes embedded for this build's version, if the release pipeline supplied them.
pub(crate) fn embedded() -> Option<&'static ReleaseNotes> {
    NOTES.as_ref()
}

/// One version's notes, reduced to what the What's new sheet shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseNotes {
    version: String,
    released: Option<ReleaseDate>,
    blocks: Vec<NotesBlock>,
}

/// One line of the notes as the sheet presents it, with Markdown markup and commit links removed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NotesBlock {
    /// A group such as "Features" or "Fixes".
    Heading(String),
    /// One change.
    Item(String),
    /// Prose outside a list, such as the first release's sentence.
    Paragraph(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReleaseDate {
    year: i32,
    month: u8,
    day: u8,
}

impl ReleaseNotes {
    /// Reads git-cliff notes for `version`, released on `released` (`YYYY-MM-DD`, or empty).
    /// Notes with no text are no notes.
    pub(crate) fn parse(version: &str, released: &str, text: &str) -> Option<Self> {
        let mut blocks = Vec::new();
        let mut paragraph: Vec<&str> = Vec::new();
        let flush = |paragraph: &mut Vec<&str>, blocks: &mut Vec<NotesBlock>| {
            if !paragraph.is_empty() {
                blocks.push(NotesBlock::Paragraph(plain(&paragraph.join(" "))));
                paragraph.clear();
            }
        };
        for line in text.lines().map(str::trim) {
            if let Some(heading) = line.strip_prefix('#') {
                flush(&mut paragraph, &mut blocks);
                let heading = plain(heading.trim_start_matches('#'));
                if !heading.is_empty() {
                    blocks.push(NotesBlock::Heading(heading));
                }
            } else if let Some(item) = list_item(line) {
                flush(&mut paragraph, &mut blocks);
                let item = plain(without_commit_link(item));
                if !item.is_empty() {
                    blocks.push(NotesBlock::Item(item));
                }
            } else if line.is_empty() {
                flush(&mut paragraph, &mut blocks);
            } else {
                paragraph.push(line);
            }
        }
        flush(&mut paragraph, &mut blocks);
        (!blocks.is_empty()).then(|| Self {
            version: version.to_owned(),
            released: ReleaseDate::parse(released),
            blocks,
        })
    }

    /// The sheet's title, such as "What's new in SpaceTerm 0.9".
    pub(crate) fn title(&self) -> String {
        format!("What's new in SpaceTerm {}", self.version)
    }

    /// The menu command that opens the sheet, such as "What's New in SpaceTerm 0.9".
    pub(crate) fn menu_title(&self) -> String {
        format!("What's New in SpaceTerm {}", self.version)
    }

    /// "Released 3 November", with the year only when it is not `this_year`.
    pub(crate) fn released(&self, this_year: i32) -> Option<String> {
        let date = self.released?;
        let month = MONTHS[usize::from(date.month - 1)];
        Some(if date.year == this_year {
            format!("Released {} {month}", date.day)
        } else {
            format!("Released {} {month} {}", date.day, date.year)
        })
    }

    pub(crate) fn blocks(&self) -> &[NotesBlock] {
        &self.blocks
    }

    /// This version's release page on GitHub.
    pub(crate) fn github_url(&self) -> String {
        format!("{}/releases/tag/v{}", crate::about::REPOSITORY, self.version)
    }
}

impl ReleaseDate {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, '-');
        let year = parts.next()?.parse().ok()?;
        let month = parts
            .next()?
            .parse()
            .ok()
            .filter(|month| (1..=12).contains(month))?;
        let day = parts
            .next()?
            .parse()
            .ok()
            .filter(|day| (1..=31).contains(day))?;
        Some(Self { year, month, day })
    }
}

/// The calendar year in UTC at `seconds` after the Unix epoch.
pub(crate) fn year_at(seconds: u64) -> i32 {
    // Howard Hinnant's days-to-civil conversion, reduced to the year.
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX / 2) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let year = year_of_era + era * 400 + i64::from(month_index >= 10);
    i32::try_from(year).unwrap_or(i32::MAX)
}

/// The text of a `-` or `*` list line, which may be empty.
fn list_item(line: &str) -> Option<&str> {
    let rest = line.strip_prefix(['-', '*'])?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim_start())
}

/// git-cliff ends each change with its commit, as ` ([abc1234](https://…))`.
fn without_commit_link(item: &str) -> &str {
    item.rfind(" ([")
        .filter(|_| item.ends_with("))"))
        .map_or(item, |start| &item[..start])
}

/// Removes emphasis and code markers, and keeps only the text of links.
fn plain(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        let link = rest[start..].find("](").and_then(|label_end| {
            let target = start + label_end + 2;
            rest[target..]
                .find(')')
                .map(|end| (label_end, target + end))
        });
        let Some((label_end, end)) = link else { break };
        plain.push_str(&rest[..start]);
        plain.push_str(&rest[start + 1..start + label_end]);
        rest = &rest[end + 1..];
    }
    plain.push_str(rest);
    plain
        .replace("**", "")
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `git-cliff --config cliff.toml --strip all` writes for one release.
    const CLIFF: &str = "### Features

- **panes:** Let panes swap places and move between Tabs ([087ed1d](https://github.com/sadiksaifi/SpaceTerm/commit/087ed1d2))
- **Breaking:** Drop the `--legacy` flag ([206c973](https://github.com/sadiksaifi/SpaceTerm/commit/206c973a))

### Fixes

- See [the guide](https://example.invalid/guide) for [details](https://example.invalid) ([1e48bad](https://github.com/sadiksaifi/SpaceTerm/commit/1e48bad1))
";

    #[test]
    fn notes_keep_groups_and_changes_without_markup_or_commit_links() {
        let notes = ReleaseNotes::parse("0.9.0", "2026-11-03", CLIFF).unwrap();
        assert_eq!(
            notes.blocks(),
            [
                NotesBlock::Heading("Features".into()),
                NotesBlock::Item("panes: Let panes swap places and move between Tabs".into()),
                NotesBlock::Item("Breaking: Drop the --legacy flag".into()),
                NotesBlock::Heading("Fixes".into()),
                NotesBlock::Item("See the guide for details".into()),
            ]
        );
    }

    #[test]
    fn a_first_release_reads_as_prose() {
        let notes = ReleaseNotes::parse("1.0.0", "", "First release\nof SpaceTerm.\n").unwrap();
        assert_eq!(
            notes.blocks(),
            [NotesBlock::Paragraph("First release of SpaceTerm.".into())]
        );
        assert_eq!(notes.released(2026), None);
    }

    /// The notes file a release embeds opens with real content: the GitHub release body alone
    /// carries the download buttons (`release_publish.release_body`).
    #[test]
    fn embedded_notes_open_with_the_release_sentence_or_the_first_change_heading() {
        let first = ReleaseNotes::parse("0.10.0", "2026-11-07", "First release of SpaceTerm.\n");
        assert_eq!(
            first.unwrap().blocks().first(),
            Some(&NotesBlock::Paragraph("First release of SpaceTerm.".into()))
        );
        let later = ReleaseNotes::parse("0.11.2", "2026-11-07", CLIFF).unwrap();
        assert_eq!(
            later.blocks().first(),
            Some(&NotesBlock::Heading("Features".into()))
        );
    }

    #[test]
    fn a_build_without_notes_has_nothing_to_show() {
        assert_eq!(ReleaseNotes::parse("0.9.0", "2026-11-03", ""), None);
        assert_eq!(
            ReleaseNotes::parse("0.9.0", "2026-11-03", "\n###\n- \n"),
            None
        );
    }

    #[test]
    fn the_sheet_names_the_version_and_its_release_day() {
        let notes = ReleaseNotes::parse("0.9.0", "2026-11-03", CLIFF).unwrap();
        assert_eq!(notes.title(), "What's new in SpaceTerm 0.9.0");
        assert_eq!(notes.menu_title(), "What's New in SpaceTerm 0.9.0");
        assert_eq!(notes.released(2026).as_deref(), Some("Released 3 November"));
        assert_eq!(
            notes.released(2027).as_deref(),
            Some("Released 3 November 2026")
        );
        let undated = ReleaseNotes::parse("0.9.0", "2026-13-03", CLIFF).unwrap();
        assert_eq!(undated.released(2026), None);
    }

    #[test]
    fn the_github_link_opens_the_release_of_the_version_shown() {
        for version in ["0.9.0", "0.11.2"] {
            let notes = ReleaseNotes::parse(version, "", CLIFF).unwrap();
            assert_eq!(
                notes.github_url(),
                format!("https://github.com/sadiksaifi/SpaceTerm/releases/tag/v{version}")
            );
        }
    }

    #[test]
    fn the_current_year_follows_the_calendar() {
        // 2026-11-04T00:00:00Z, 2024-12-31T23:59:59Z and 2025-01-01T00:00:00Z.
        assert_eq!(year_at(1_793_750_400), 2026);
        assert_eq!(year_at(1_735_689_599), 2024);
        assert_eq!(year_at(1_735_689_600), 2025);
        assert_eq!(year_at(0), 1970);
    }
}
