//! Labels shared by desktop captions and CLI handles.
use crate::{hierarchy, pane::metadata};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    pub activity: String,
    pub line: String,
}

impl Label {
    /// The place is the directory leaf; identical activity and place appear only once.
    pub fn from_parts(activity: &str, directory: &str) -> Self {
        let activity = if activity.is_empty() {
            "Terminal"
        } else {
            activity
        };
        let trimmed = directory.trim_end_matches('/');
        let place = if trimmed.is_empty() {
            directory
        } else {
            trimmed.rsplit('/').next().unwrap_or(trimmed)
        };
        let line = if place.is_empty() || place == activity {
            activity.to_owned()
        } else {
            format!("{activity} · {place}")
        };
        Self {
            activity: activity.to_owned(),
            line,
        }
    }

    pub fn place(&self) -> &str {
        self.line
            .strip_prefix(&self.activity)
            .and_then(|s| s.strip_prefix(" · "))
            .unwrap_or("")
    }
}

/// Terminal-controlled titles may donate a glyph. Commands remain opaque.
pub fn activity(title: &str, terminal_control: bool, running: Option<&str>, shell: &str) -> String {
    if terminal_control {
        return reported_title(&metadata::sanitize(title, metadata::MAX_TITLE_CHARS))
            .words
            .to_owned();
    }
    if let Some(command) = running {
        let command = metadata::sanitize(command, metadata::MAX_TITLE_CHARS);
        if !command.is_empty() {
            return command;
        }
    }
    let shell = metadata::sanitize(shell.trim(), metadata::MAX_TITLE_CHARS);
    if shell.is_empty() {
        "Terminal".into()
    } else {
        shell
    }
}

fn presentation(snapshot: &metadata::Snapshot) -> Label {
    let running = snapshot
        .command
        .as_ref()
        .filter(|c| c.state == metadata::CommandState::Running);
    let activity = activity(
        &snapshot.title,
        snapshot.title_provenance == metadata::TitleProvenance::TerminalControl,
        running.map(|c| c.line.as_str()),
        &snapshot.shell_name,
    );
    let directory = compact_home_directory(&snapshot.directory, snapshot.home.as_deref());
    Label::from_parts(&activity, &directory)
}

/// A Pane without terminal metadata uses the same caption fallback on every client.
pub fn pane(pane: &hierarchy::Pane) -> Label {
    pane.presentation
        .as_deref()
        .map(presentation)
        .unwrap_or_else(|| Label::from_parts("Terminal", ""))
}

/// A Tab uses its Root Pane's identity. Every Pane remains independently searchable.
pub fn tab(tab: &hierarchy::Tab) -> Label {
    tab.panes
        .iter()
        .find(|p| p.id == tab.root_pane)
        .map(pane)
        .unwrap_or_else(|| Label::from_parts("Terminal", ""))
}

pub fn compact_home_directory(directory: &str, home: Option<&str>) -> String {
    let Some(home) = home
        .map(|h| h.trim_end_matches('/'))
        .filter(|h| !h.is_empty() && directory.starts_with(h))
    else {
        return directory.to_owned();
    };
    match &directory[home.len()..] {
        "" | "/" => "~".to_owned(),
        rest if rest.starts_with('/') => format!("~{rest}"),
        _ => directory.to_owned(),
    }
}

/// How many characters the first word of a title can hold and still be a glyph rather than a word.
const MAXIMUM_GLYPH_CHARS: usize = 2;

/// Characters a title keeps, because a Terminal Session can be named after them.
const TITLE_WORD_CHARS: &str = "~/\\._-:@$#([{'\"";

/// Characters that separate a program's glyph from the words after it.
const TITLE_SEPARATOR_CHARS: &str = "-\u{2013}\u{2014}\u{00b7}|:";

/// What a program put at the front of the title it reported, and the words that follow it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReportedTitle<'a> {
    /// The program's own glyph, when it reported one host chrome can draw.
    pub glyph: Option<&'a str>,
    /// The title without that glyph.
    pub words: &'a str,
}

/// Splits a decorative title prefix from its words. The chrome font's shaper decides whether
/// the candidate can occupy the Terminal Session's glyph slot.
pub fn reported_title(title: &str) -> ReportedTitle<'_> {
    let title = title.trim();
    let plain = ReportedTitle {
        glyph: None,
        words: title,
    };
    let Some(split) = title.find(char::is_whitespace) else {
        return plain;
    };
    let (first, rest) = title.split_at(split);
    if !is_glyph_word(first) {
        return plain;
    }
    let words = title_words_after_glyph(rest);
    if words.is_empty() {
        return plain;
    }
    ReportedTitle {
        glyph: is_glyph_candidate(first).then_some(first),
        words,
    }
}

/// Removes whitespace and one standalone delimiter token after a reported glyph.
///
/// Punctuation attached to the next opaque word is content, such as `--help` or `:memory`.
fn title_words_after_glyph(rest: &str) -> &str {
    let words = rest.trim_start();
    let Some(split) = words.find(char::is_whitespace) else {
        return words.trim_end();
    };
    let (first, remainder) = words.split_at(split);
    if first.chars().count() == 1
        && first
            .chars()
            .all(|character| TITLE_SEPARATOR_CHARS.contains(character))
    {
        remainder.trim()
    } else {
        words.trim_end()
    }
}

/// Whether the first word of a title decorates it rather than naming what the Terminal Session is doing.
fn is_glyph_word(word: &str) -> bool {
    let bases = word
        .chars()
        .filter(|character| !is_glyph_mark(*character))
        .count();
    // Several letters name something, in whatever script they are written. Several symbols
    // together are still decoration.
    let decorative = bases == 1 || word.chars().all(|character| !character.is_alphanumeric());
    (1..=MAXIMUM_GLYPH_CHARS).contains(&bases)
        && decorative
        && word.chars().all(|character| {
            !character.is_ascii_alphanumeric() && !TITLE_WORD_CHARS.contains(character)
        })
}

/// Whether a character belongs to the glyph before it rather than standing as one of its own.
pub fn is_glyph_mark(character: char) -> bool {
    matches!(
        u32::from(character),
        // Variation selectors, the zero-width joiner, and the skin tone modifiers.
        0xFE00..=0xFE0F | 0x200D | 0x1F3FB..=0x1F3FF | 0xE0100..=0xE01EF
    )
}

/// Whether a title prefix has the structural shape of a reported glyph candidate.
///
/// A Private-Use character is drawn by the font a program expects rather than by the font the host
/// paints its chrome in, so it would paint as a missing-glyph box. An ASCII one says less than the
/// Terminal Session's own glyph does. Actual Chrome-font support is checked after shaping.
fn is_glyph_candidate(glyph: &str) -> bool {
    glyph.chars().all(|character| {
        !character.is_ascii()
            && !matches!(
                u32::from(character),
                0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> metadata::Snapshot {
        serde_json::from_value(serde_json::json!({
            "revision": 1, "shell_name": "zsh", "title": "api",
            "title_provenance": "WorkingDirectory", "directory": "/home/test/api",
            "directory_provenance": "Osc7", "user": null, "host": null,
            "home": "/home/test", "freshness": "Live", "finished_commands": 0,
            "prompt_zone": "CommandOutput", "command": {"line": "cargo test", "state": "Running"},
            "title_activity": false, "title_glyph": "Reported", "progress": "None"
        }))
        .unwrap()
    }

    #[test]
    fn tab_identity_uses_the_root_pane_in_any_position_and_handles_missing_metadata() {
        let mut state = hierarchy::State::empty();
        state
            .apply(
                [1; 32],
                1,
                hierarchy::Mutation::CreateWorkspace {
                    name: "test".into(),
                    directory: None,
                    control: hierarchy::CreateControl::Creator,
                },
                std::path::Path::new("/tmp"),
                |_| true,
            )
            .unwrap();
        let mut tab = state.inventory.workspaces[0].tabs[0].clone();
        let root = tab.root_pane;
        tab.panes[0].presentation = Some(Box::new(snapshot()));
        let mut other = tab.panes[0].clone();
        other.id = [8; 32];
        other.presentation = None;
        tab.panes.insert(0, other);
        assert_eq!(self::tab(&tab).line, "cargo test · api");
        assert_eq!(pane(&tab.panes[0]).line, "Terminal");
        tab.panes
            .iter_mut()
            .find(|p| p.id == root)
            .unwrap()
            .presentation = None;
        assert_eq!(self::tab(&tab).line, "Terminal");
        tab.panes.retain(|p| p.id != root);
        assert_eq!(self::tab(&tab).line, "Terminal");
    }

    #[test]
    fn labels_follow_title_command_and_shell_precedence() {
        let mut pane = snapshot();
        assert_eq!(
            presentation(&pane),
            Label {
                activity: "cargo test".into(),
                line: "cargo test · api".into()
            }
        );
        pane.title_provenance = metadata::TitleProvenance::TerminalControl;
        pane.title = "⚡ - Editor".into();
        assert_eq!(presentation(&pane).line, "Editor · api");
        pane.title_provenance = metadata::TitleProvenance::WorkingDirectory;
        pane.command = None;
        assert_eq!(presentation(&pane).line, "zsh · api");
        pane.shell_name.clear();
        assert_eq!(presentation(&pane).activity, "Terminal");
    }

    #[test]
    fn places_preserve_home_root_and_deduplicate_the_activity() {
        let mut pane = snapshot();
        pane.directory = "/home/test".into();
        assert_eq!(presentation(&pane).line, "cargo test · ~");
        pane.directory = "/".into();
        assert_eq!(presentation(&pane).line, "cargo test · /");
        pane.title_provenance = metadata::TitleProvenance::TerminalControl;
        pane.title = "api".into();
        pane.directory = "/home/test/api/".into();
        assert_eq!(presentation(&pane).line, "api");
        pane.directory.clear();
        assert_eq!(presentation(&pane).line, "api");
    }
}
