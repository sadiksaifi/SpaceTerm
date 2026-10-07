//! Repository Status presentation: the text and structure each surface shows for one view.
//!
//! Everything here is pure. Every repository-supplied string is sanitized before display, and no
//! state is carried by color alone: a dimmed value also says why in its accessible label.

use std::sync::Arc;
use std::time::Instant;

use super::{
    ChangeKind, ChangeState, ChangeSummary, ChangeTotal, Freshness, PullRequest, RepositoryHead,
    RepositoryOperation, RepositoryStatus, RepositoryView,
};

/// The icon before a branch name.
pub(crate) const BRANCH_GLYPH: &str = "⎇";
/// The icon before a detached commit id.
pub(crate) const DETACHED_GLYPH: &str = "◆";

/// The longest branch, upstream, commit, or repository name displayed, in characters.
pub(crate) const MAXIMUM_NAME_CHARS: usize = 100;
/// The longest Pull Request title displayed, in characters.
pub(crate) const MAXIMUM_TITLE_CHARS: usize = 200;
/// The longest repository path or changed path displayed, in characters.
pub(crate) const MAXIMUM_PATH_CHARS: usize = 240;

const REPLACEMENT: char = '\u{FFFD}';
const ELLIPSIS: char = '…';

/// Replaces control and bidirectional formatting characters with U+FFFD and bounds the text to
/// `maximum_chars`, ending a cut with an ellipsis.
pub(crate) fn sanitize_for_display(text: &str, maximum_chars: usize) -> String {
    let sanitized: Vec<char> = text
        .chars()
        .map(|character| {
            if character.is_control() || is_bidirectional_format(character) {
                REPLACEMENT
            } else {
                character
            }
        })
        .collect();
    if sanitized.len() <= maximum_chars {
        return sanitized.into_iter().collect();
    }
    if maximum_chars == 0 {
        return String::new();
    }
    let mut bounded: String = sanitized[..maximum_chars - 1].iter().collect();
    bounded.push(ELLIPSIS);
    bounded
}

/// How long ago `then` was, as of `now`: "just now", "5 minutes ago", "yesterday".
pub(crate) fn age_text(then: Instant, now: Instant) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match now.saturating_duration_since(then).as_secs() {
        age if age < MINUTE => "just now".to_owned(),
        age if age < HOUR => format!("{} ago", plural(age / MINUTE, "minute")),
        age if age < DAY => format!("{} ago", plural(age / HOUR, "hour")),
        age if age < 2 * DAY => "yesterday".to_owned(),
        age => format!("{} ago", plural(age / DAY, "day")),
    }
}

/// The one state mark a caption carries, in precedence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RepositoryMark {
    /// An unfinished git operation.
    Operation,
    /// Uncommitted changes.
    Changes,
}

impl RepositoryMark {
    pub(crate) const fn glyph(self) -> &'static str {
        match self {
            Self::Operation => "▲",
            Self::Changes => "●",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CaptionDetail {
    pub(crate) text: String,
    /// The detail is a pending count, presented dimmed.
    pub(crate) dimmed: bool,
}

/// The Pane Caption's Repository Status segment, split into the parts it drops independently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepositoryCaption {
    /// [`BRANCH_GLYPH`] or [`DETACHED_GLYPH`].
    pub(crate) glyph: &'static str,
    /// The branch name or detached commit id.
    pub(crate) branch: String,
    pub(crate) mark: Option<RepositoryMark>,
    /// The change count, the operation, or "No commits".
    pub(crate) detail: Option<CaptionDetail>,
    /// Ahead and behind the upstream, such as `↑1 ↓2`.
    pub(crate) divergence: Option<String>,
    /// The whole segment is last known.
    pub(crate) dimmed: bool,
    /// The full spoken label of the segment's button.
    pub(crate) accessible_label: String,
}

impl RepositoryCaption {
    /// The caption segment for a view, or `None` when Repository Status shows nothing.
    pub(crate) fn from_view(view: &RepositoryView, now: Instant) -> Option<Self> {
        let status = presented(view)?;
        let (glyph, branch) = head_text(&status.head);
        let detail = caption_detail(status);
        let mark = if status.operation.is_some() {
            Some(RepositoryMark::Operation)
        } else if detail.as_ref().is_some_and(|detail| !detail.dimmed)
            && !matches!(status.head, RepositoryHead::Unborn(_))
        {
            Some(RepositoryMark::Changes)
        } else {
            None
        };
        Some(Self {
            glyph,
            branch,
            mark,
            detail,
            divergence: status.upstream.as_ref().and_then(divergence_text),
            dimmed: matches!(status.freshness, Freshness::LastKnown { .. }),
            accessible_label: accessible_label(status, now),
        })
    }

    /// The whole segment as one line, in the order the caption paints it.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        let mut parts = vec![self.glyph, &self.branch];
        if let Some(mark) = self.mark {
            parts.push(mark.glyph());
        }
        if let Some(detail) = &self.detail {
            parts.push(&detail.text);
        }
        if let Some(divergence) = &self.divergence {
            parts.push(divergence);
        }
        parts.join(" ")
    }
}

/// The Repository Status popover's rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepositoryPopover {
    /// The repository root's last component.
    pub(crate) name: String,
    /// The repository root.
    pub(crate) root: String,
    /// The branch, detached commit, or unborn branch.
    pub(crate) branch: String,
    pub(crate) operation: Option<String>,
    pub(crate) pull_request: Option<PopoverPullRequest>,
    pub(crate) upstream: Option<PopoverUpstream>,
    /// The abbreviated commit id.
    pub(crate) commit: Option<String>,
    /// `None` until a count finishes.
    pub(crate) changes: Option<PopoverChanges>,
    pub(crate) footer: String,
    pub(crate) dimmed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PopoverPullRequest {
    /// Such as `#478`.
    pub(crate) number: String,
    pub(crate) state: PullRequestState,
    pub(crate) title: String,
    /// The `https` URL the number opens.
    pub(crate) url: Arc<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PopoverUpstream {
    pub(crate) name: String,
    /// `↑1 ↓2`, "Upstream gone", or `None` when even.
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PopoverChanges {
    /// Such as "1 staged, 2 modified", or "No changes".
    pub(crate) summary: String,
    pub(crate) entries: Vec<PopoverChange>,
    /// Such as "and 14 more".
    pub(crate) more: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PopoverChange {
    /// One letter for the kind, such as `S` or `M`.
    pub(crate) letter: &'static str,
    /// The kind in words, for the accessible label.
    pub(crate) kind: &'static str,
    pub(crate) path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PullRequestState {
    Open,
    Draft,
}

impl PullRequestState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Draft => "Draft",
        }
    }
}

impl RepositoryPopover {
    pub(crate) fn from_view(view: &RepositoryView, now: Instant) -> Option<Self> {
        let status = presented(view)?;
        let root = status.key.root.display();
        let trimmed = root.trim_end_matches('/');
        let name = trimmed.rsplit('/').next().filter(|name| !name.is_empty());
        let branch = match &status.head {
            RepositoryHead::Branch(branch) => name_text(branch),
            RepositoryHead::Detached(commit) => format!("Detached at {}", name_text(commit)),
            RepositoryHead::Unborn(branch) => format!("{} (no commits)", name_text(branch)),
        };
        let changes = match &status.changes {
            ChangeState::Known(changes) => Some(popover_changes(changes)),
            ChangeState::NotCounted | ChangeState::Counting => None,
        };
        Some(Self {
            name: sanitize_for_display(name.unwrap_or(&root), MAXIMUM_NAME_CHARS),
            root: sanitize_for_display(&root, MAXIMUM_PATH_CHARS),
            branch,
            operation: status
                .operation
                .map(|operation| operation_text(operation, &status.changes)),
            pull_request: status
                .pull_request
                .as_ref()
                .map(|pull_request| PopoverPullRequest {
                    number: format!("#{}", pull_request.number),
                    state: pull_request_state(pull_request),
                    title: sanitize_for_display(&pull_request.title, MAXIMUM_TITLE_CHARS),
                    url: pull_request.url.clone(),
                }),
            upstream: status.upstream.as_ref().map(|upstream| PopoverUpstream {
                name: name_text(&upstream.name),
                detail: match upstream.divergence {
                    None => Some("Upstream gone".to_owned()),
                    Some(_) => divergence_text(upstream),
                },
            }),
            commit: status.commit.as_deref().map(name_text),
            changes,
            footer: footer(status, now),
            dimmed: matches!(status.freshness, Freshness::LastKnown { .. }),
        })
    }
}

/// What a Workspace sidebar row's second line shows before its directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SidebarBadge {
    /// Such as `⎇ main`.
    Branch { text: String },
    /// An open or draft Pull Request replaces the branch.
    PullRequest {
        /// Such as `#478`.
        text: String,
        state: PullRequestState,
        url: Arc<str>,
        hover: PullRequestHover,
        accessible_label: String,
    },
}

/// The cached Pull Request facts a hover shows. Hovering makes no lookup of its own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestHover {
    /// Such as `#478 · Draft`.
    pub(crate) heading: String,
    pub(crate) title: String,
    /// Such as `issue-441 into main`.
    pub(crate) branches: String,
}

impl PullRequestHover {
    pub(crate) fn lines(&self) -> [&str; 3] {
        [&self.heading, &self.title, &self.branches]
    }
}

impl SidebarBadge {
    /// The branch line or the Pull Request number.
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Branch { text } | Self::PullRequest { text, .. } => text,
        }
    }

    pub(crate) fn from_view(view: &RepositoryView) -> Option<Self> {
        let status = presented(view)?;
        let Some(pull_request) = &status.pull_request else {
            return Some(Self::Branch {
                text: branch_line(&status.head),
            });
        };
        let state = pull_request_state(pull_request);
        let title = sanitize_for_display(&pull_request.title, MAXIMUM_TITLE_CHARS);
        Some(Self::PullRequest {
            text: format!("#{}", pull_request.number),
            state,
            url: pull_request.url.clone(),
            accessible_label: format!(
                "Pull request {}, {}: {title}",
                pull_request.number,
                state.label().to_lowercase()
            ),
            hover: PullRequestHover {
                heading: format!("#{} · {}", pull_request.number, state.label()),
                title,
                branches: format!(
                    "{} into {}",
                    name_text(&pull_request.head),
                    name_text(&pull_request.base)
                ),
            },
        })
    }
}

/// The collapsed title-bar chip's tooltip line, such as `⎇ main`.
pub(crate) fn chip_tooltip_line(view: &RepositoryView) -> Option<String> {
    presented(view).map(|status| branch_line(&status.head))
}

fn presented(view: &RepositoryView) -> Option<&RepositoryStatus> {
    match view {
        RepositoryView::Hidden => None,
        RepositoryView::Repository(status) => Some(status),
    }
}

fn is_bidirectional_format(character: char) -> bool {
    matches!(
        character,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

fn plural(count: impl Into<u64>, unit: &str) -> String {
    match count.into() {
        1 => format!("1 {unit}"),
        count => format!("{count} {unit}s"),
    }
}

fn name_text(name: &str) -> String {
    sanitize_for_display(name, MAXIMUM_NAME_CHARS)
}

/// The glyph and the branch name or detached commit id.
fn head_text(head: &RepositoryHead) -> (&'static str, String) {
    match head {
        RepositoryHead::Branch(branch) | RepositoryHead::Unborn(branch) => {
            (BRANCH_GLYPH, name_text(branch))
        }
        RepositoryHead::Detached(commit) => (DETACHED_GLYPH, name_text(commit)),
    }
}

/// The head as one line, such as `⎇ main`.
fn branch_line(head: &RepositoryHead) -> String {
    let (glyph, branch) = head_text(head);
    format!("{glyph} {branch}")
}

/// An operation replaces the count, an unborn branch says so, and a slow first count is dimmed.
fn caption_detail(status: &RepositoryStatus) -> Option<CaptionDetail> {
    let detail = |text: String| CaptionDetail {
        text,
        dimmed: false,
    };
    if let Some(operation) = status.operation {
        return Some(detail(operation_text(operation, &status.changes)));
    }
    if matches!(status.head, RepositoryHead::Unborn(_)) {
        return Some(detail("No commits".to_owned()));
    }
    match &status.changes {
        ChangeState::NotCounted => None,
        ChangeState::Counting => Some(CaptionDetail {
            text: "counting…".to_owned(),
            dimmed: true,
        }),
        ChangeState::Known(changes) => match changes.total {
            ChangeTotal::Exact(0) | ChangeTotal::AtLeast(0) => None,
            ChangeTotal::Exact(total) => Some(detail(plural(total, "change"))),
            ChangeTotal::AtLeast(total) => Some(detail(format!("{total}+ changes"))),
        },
    }
}

/// Such as `Rebasing 3/7` or `Merging · 2 conflicts`.
fn operation_text(operation: RepositoryOperation, changes: &ChangeState) -> String {
    let (name, step) = operation_name(operation);
    let mut text = name.to_owned();
    if let Some(step) = step {
        text.push_str(&format!(" {}/{}", step.current, step.total));
    }
    if let Some(conflicts) = conflicts(changes) {
        text.push_str(&format!(" · {}", plural(conflicts, "conflict")));
    }
    text
}

fn operation_name(operation: RepositoryOperation) -> (&'static str, Option<super::OperationStep>) {
    match operation {
        RepositoryOperation::Rebasing { step } => ("Rebasing", step),
        RepositoryOperation::Applying { step } => ("Applying", step),
        RepositoryOperation::Merging => ("Merging", None),
        RepositoryOperation::Reverting => ("Reverting", None),
        RepositoryOperation::CherryPicking => ("Cherry-picking", None),
        RepositoryOperation::Bisecting => ("Bisecting", None),
    }
}

fn conflicts(changes: &ChangeState) -> Option<u32> {
    match changes {
        ChangeState::Known(changes) if changes.conflicted > 0 => Some(changes.conflicted),
        _ => None,
    }
}

/// Such as `↑1 ↓2`, omitting zeros. `None` when even or when the upstream is gone.
fn divergence_text(upstream: &super::Upstream) -> Option<String> {
    let divergence = upstream.divergence?;
    let mut parts = Vec::new();
    if divergence.ahead > 0 {
        parts.push(format!("↑{}", divergence.ahead));
    }
    if divergence.behind > 0 {
        parts.push(format!("↓{}", divergence.behind));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn accessible_label(status: &RepositoryStatus, now: Instant) -> String {
    let mut parts = vec![match &status.head {
        RepositoryHead::Branch(branch) | RepositoryHead::Unborn(branch) => {
            format!("branch {}", name_text(branch))
        }
        RepositoryHead::Detached(commit) => format!("detached at {}", name_text(commit)),
    }];
    if let Some(operation) = status.operation {
        let (name, step) = operation_name(operation);
        parts.push(name.to_lowercase());
        if let Some(step) = step {
            parts.push(format!("step {} of {}", step.current, step.total));
        }
        if let Some(conflicts) = conflicts(&status.changes) {
            parts.push(plural(conflicts, "conflict"));
        }
    } else if matches!(status.head, RepositoryHead::Unborn(_)) {
        parts.push("no commits".to_owned());
    } else {
        match &status.changes {
            ChangeState::NotCounted => {}
            ChangeState::Counting => parts.push("counting changes".to_owned()),
            ChangeState::Known(changes) => match changes.total {
                ChangeTotal::Exact(0) => parts.push("no changes".to_owned()),
                ChangeTotal::Exact(total) => parts.push(plural(total, "change")),
                ChangeTotal::AtLeast(0) => {}
                ChangeTotal::AtLeast(total) => parts.push(format!("at least {total} changes")),
            },
        }
    }
    if let Some(divergence) = status
        .upstream
        .as_ref()
        .and_then(|upstream| upstream.divergence)
    {
        if divergence.ahead > 0 {
            parts.push(format!("{} ahead", divergence.ahead));
        }
        if divergence.behind > 0 {
            parts.push(format!("{} behind", divergence.behind));
        }
    }
    let mut label = format!("Repository status: {}.", parts.join(", "));
    if let Freshness::LastKnown { as_of } = status.freshness {
        let last_known = format!("Last known {}.", age_text(as_of, now));
        label.push(' ');
        if status.read_failure.is_some() {
            label.push_str(&format!("Couldn't read repository status. {last_known}"));
        } else {
            label.push_str(&last_known);
        }
    }
    label
}

fn footer(status: &RepositoryStatus, now: Instant) -> String {
    match status.freshness {
        Freshness::LastKnown { as_of } if status.read_failure.is_some() => format!(
            "Couldn't read repository status. Last known {}.",
            age_text(as_of, now)
        ),
        Freshness::LastKnown { as_of } => format!("Last known {}", age_text(as_of, now)),
        Freshness::Current if status.changes == ChangeState::Counting => {
            "Counting changes…".to_owned()
        }
        Freshness::Current => format!("Updated {}", age_text(status.read_at, now)),
    }
}

fn popover_changes(changes: &ChangeSummary) -> PopoverChanges {
    let summary = match changes.total {
        ChangeTotal::AtLeast(total) => format!("{total}+ changes"),
        ChangeTotal::Exact(0) => "No changes".to_owned(),
        ChangeTotal::Exact(total) => {
            let kinds: Vec<String> = [
                (changes.conflicted, "conflicted"),
                (changes.staged, "staged"),
                (changes.renamed, "renamed"),
                (changes.deleted, "deleted"),
                (changes.modified, "modified"),
                (changes.untracked, "untracked"),
            ]
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, kind)| format!("{count} {kind}"))
            .collect();
            if kinds.is_empty() {
                plural(total, "change")
            } else {
                kinds.join(", ")
            }
        }
    };
    let listed = u32::try_from(changes.entries.len()).unwrap_or(u32::MAX);
    let more = match changes.total {
        ChangeTotal::Exact(total) if total > listed => Some(format!("and {} more", total - listed)),
        ChangeTotal::AtLeast(total) if total > listed => {
            Some(format!("and {}+ more", total - listed))
        }
        _ => None,
    };
    PopoverChanges {
        summary,
        entries: changes
            .entries
            .iter()
            .map(|entry| {
                let (letter, kind) = change_kind_text(entry.kind);
                PopoverChange {
                    letter,
                    kind,
                    path: sanitize_for_display(&entry.path, MAXIMUM_PATH_CHARS),
                }
            })
            .collect(),
        more,
    }
}

fn change_kind_text(kind: ChangeKind) -> (&'static str, &'static str) {
    match kind {
        ChangeKind::Staged => ("S", "Staged"),
        ChangeKind::Modified => ("M", "Modified"),
        ChangeKind::Deleted => ("D", "Deleted"),
        ChangeKind::Renamed => ("R", "Renamed"),
        ChangeKind::Conflicted => ("U", "Conflicted"),
        ChangeKind::Untracked => ("?", "Untracked"),
    }
}

fn pull_request_state(pull_request: &PullRequest) -> PullRequestState {
    if pull_request.draft {
        PullRequestState::Draft
    } else {
        PullRequestState::Open
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::repository_status::{
        ChangeEntry, Divergence, OperationStep, RepositoryKey, RepositoryMachine,
        RepositoryReadError, RepositoryRoot, Upstream,
    };

    fn status(head: RepositoryHead, changes: ChangeState, read_at: Instant) -> RepositoryStatus {
        RepositoryStatus {
            key: RepositoryKey {
                machine: RepositoryMachine::Local,
                root: RepositoryRoot::Local(PathBuf::from("/Users/sdk/Projects/SpaceTerm")),
            },
            head,
            commit: Some("a1b2c3d".into()),
            upstream: None,
            operation: None,
            changes,
            freshness: Freshness::Current,
            read_at,
            pull_request: None,
            read_failure: None,
        }
    }

    fn main_branch() -> RepositoryHead {
        RepositoryHead::Branch("main".into())
    }

    fn modified(total: u32) -> ChangeState {
        ChangeState::Known(ChangeSummary {
            total: ChangeTotal::Exact(total),
            modified: total,
            ..ChangeSummary::default()
        })
    }

    fn view(status: RepositoryStatus) -> RepositoryView {
        RepositoryView::Repository(Arc::new(status))
    }

    fn upstream(ahead: u32, behind: u32) -> Option<Upstream> {
        Some(Upstream {
            name: "origin/main".into(),
            divergence: Some(Divergence { ahead, behind }),
        })
    }

    fn caption(status: RepositoryStatus, now: Instant) -> RepositoryCaption {
        RepositoryCaption::from_view(&view(status), now).expect("a caption")
    }

    fn line(status: RepositoryStatus) -> String {
        let now = status.read_at;
        caption(status, now).text()
    }

    fn pull_request(draft: bool) -> PullRequest {
        PullRequest {
            number: 478,
            title: "Show Repository Status".into(),
            draft,
            url: "https://github.com/sadiksaifi/spaceterm/pull/478".into(),
            head: "issue-441".into(),
            base: "main".into(),
        }
    }

    #[test]
    fn sanitizing_replaces_control_and_bidirectional_characters() {
        assert_eq!(
            sanitize_for_display("ma\u{1b}[31min\u{202e}txt\u{2066}\n", 100),
            "ma\u{FFFD}[31min\u{FFFD}txt\u{FFFD}\u{FFFD}"
        );
        assert_eq!(
            sanitize_for_display("feature/ünïcode", 100),
            "feature/ünïcode"
        );
    }

    #[test]
    fn sanitizing_bounds_length_with_an_ellipsis() {
        assert_eq!(sanitize_for_display("abcdef", 6), "abcdef");
        assert_eq!(sanitize_for_display("abcdefg", 6), "abcde…");
        assert_eq!(sanitize_for_display("abc", 0), "");
    }

    #[test]
    fn ages_read_in_plain_words() {
        let then = Instant::now();
        let after = |seconds| then + Duration::from_secs(seconds);
        assert_eq!(age_text(then, then), "just now");
        assert_eq!(age_text(then, after(59)), "just now");
        assert_eq!(age_text(then, after(60)), "1 minute ago");
        assert_eq!(age_text(then, after(5 * 60)), "5 minutes ago");
        assert_eq!(age_text(then, after(3_600)), "1 hour ago");
        assert_eq!(age_text(then, after(2 * 3_600)), "2 hours ago");
        assert_eq!(age_text(then, after(86_400)), "yesterday");
        assert_eq!(age_text(then, after(3 * 86_400)), "3 days ago");
        assert_eq!(age_text(after(10), then), "just now");
    }

    #[test]
    fn hidden_views_present_nothing() {
        let now = Instant::now();
        assert_eq!(
            RepositoryCaption::from_view(&RepositoryView::Hidden, now),
            None
        );
        assert_eq!(
            RepositoryPopover::from_view(&RepositoryView::Hidden, now),
            None
        );
        assert_eq!(SidebarBadge::from_view(&RepositoryView::Hidden), None);
        assert_eq!(chip_tooltip_line(&RepositoryView::Hidden), None);
    }

    #[test]
    fn the_caption_counts_changes_in_plain_words() {
        let now = Instant::now();
        let mut repository = status(main_branch(), modified(4), now);
        repository.upstream = upstream(1, 2);
        assert_eq!(line(repository.clone()), "⎇ main ● 4 changes ↑1 ↓2");
        assert_eq!(caption(repository, now).mark, Some(RepositoryMark::Changes));

        assert_eq!(
            line(status(main_branch(), modified(1), now)),
            "⎇ main ● 1 change"
        );
        assert_eq!(line(status(main_branch(), modified(0), now)), "⎇ main");
        let truncated = ChangeState::Known(ChangeSummary {
            total: ChangeTotal::AtLeast(3_000),
            ..ChangeSummary::default()
        });
        assert_eq!(
            line(status(main_branch(), truncated, now)),
            "⎇ main ● 3000+ changes"
        );
    }

    #[test]
    fn the_caption_omits_even_divergence_and_a_gone_upstream() {
        let now = Instant::now();
        let mut repository = status(main_branch(), modified(0), now);
        repository.upstream = upstream(0, 2);
        assert_eq!(line(repository.clone()), "⎇ main ↓2");
        repository.upstream = upstream(3, 0);
        assert_eq!(line(repository.clone()), "⎇ main ↑3");
        repository.upstream = upstream(0, 0);
        assert_eq!(line(repository.clone()), "⎇ main");
        repository.upstream = Some(Upstream {
            name: "origin/gone".into(),
            divergence: None,
        });
        assert_eq!(line(repository), "⎇ main");
    }

    #[test]
    fn detached_and_unborn_heads_name_themselves() {
        let now = Instant::now();
        assert_eq!(
            line(status(
                RepositoryHead::Detached("a1b2c3d".into()),
                modified(2),
                now
            )),
            "◆ a1b2c3d ● 2 changes"
        );
        let unborn = caption(
            status(RepositoryHead::Unborn("main".into()), modified(1), now),
            now,
        );
        assert_eq!(unborn.text(), "⎇ main No commits");
        assert_eq!(unborn.mark, None);
    }

    #[test]
    fn operations_replace_the_count_and_carry_the_warning_mark() {
        let now = Instant::now();
        let with = |operation, changes| {
            let mut repository = status(main_branch(), changes, now);
            repository.operation = Some(operation);
            line(repository)
        };
        let step = |current, total| Some(OperationStep { current, total });
        assert_eq!(
            with(
                RepositoryOperation::Rebasing { step: step(3, 7) },
                modified(4)
            ),
            "⎇ main ▲ Rebasing 3/7"
        );
        assert_eq!(
            with(RepositoryOperation::Rebasing { step: None }, modified(0)),
            "⎇ main ▲ Rebasing"
        );
        assert_eq!(
            with(
                RepositoryOperation::Applying { step: step(2, 5) },
                modified(0)
            ),
            "⎇ main ▲ Applying 2/5"
        );
        let conflicts = ChangeState::Known(ChangeSummary {
            total: ChangeTotal::Exact(3),
            conflicted: 2,
            ..ChangeSummary::default()
        });
        assert_eq!(
            with(RepositoryOperation::Merging, conflicts),
            "⎇ main ▲ Merging · 2 conflicts"
        );
        assert_eq!(
            with(RepositoryOperation::Merging, modified(1)),
            "⎇ main ▲ Merging"
        );
        assert_eq!(
            with(RepositoryOperation::CherryPicking, modified(0)),
            "⎇ main ▲ Cherry-picking"
        );
        assert_eq!(
            with(RepositoryOperation::Reverting, modified(0)),
            "⎇ main ▲ Reverting"
        );
        assert_eq!(
            with(RepositoryOperation::Bisecting, modified(0)),
            "⎇ main ▲ Bisecting"
        );
    }

    #[test]
    fn a_slow_count_shows_counting_dimmed() {
        let now = Instant::now();
        let mut repository = status(main_branch(), ChangeState::Counting, now);
        repository.upstream = upstream(1, 0);
        let counting = caption(repository, now);
        assert_eq!(counting.text(), "⎇ main counting… ↑1");
        assert_eq!(
            counting.detail,
            Some(CaptionDetail {
                text: "counting…".to_owned(),
                dimmed: true
            })
        );
        assert_eq!(counting.mark, None);
        assert!(!counting.dimmed);
        assert_eq!(
            line(status(main_branch(), ChangeState::NotCounted, now)),
            "⎇ main"
        );
    }

    #[test]
    fn a_last_known_caption_is_dimmed() {
        let now = Instant::now();
        let mut repository = status(main_branch(), modified(1), now);
        repository.freshness = Freshness::LastKnown { as_of: now };
        assert!(caption(repository, now).dimmed);
    }

    #[test]
    fn the_accessible_label_speaks_every_state() {
        let now = Instant::now();
        let mut repository = status(main_branch(), modified(4), now);
        repository.upstream = upstream(1, 2);
        repository.freshness = Freshness::LastKnown { as_of: now };
        assert_eq!(
            caption(repository.clone(), now + Duration::from_secs(300)).accessible_label,
            "Repository status: branch main, 4 changes, 1 ahead, 2 behind. \
             Last known 5 minutes ago."
        );

        repository.freshness = Freshness::Current;
        repository.upstream = upstream(0, 0);
        assert_eq!(
            caption(repository.clone(), now).accessible_label,
            "Repository status: branch main, 4 changes."
        );

        repository.operation = Some(RepositoryOperation::Rebasing {
            step: Some(OperationStep {
                current: 3,
                total: 7,
            }),
        });
        repository.read_failure = Some(RepositoryReadError::Unavailable);
        repository.freshness = Freshness::LastKnown { as_of: now };
        assert_eq!(
            caption(repository, now).accessible_label,
            "Repository status: branch main, rebasing, step 3 of 7. \
             Couldn't read repository status. Last known just now."
        );

        let detached = status(
            RepositoryHead::Detached("a1b2c3d".into()),
            ChangeState::Counting,
            now,
        );
        assert_eq!(
            caption(detached, now).accessible_label,
            "Repository status: detached at a1b2c3d, counting changes."
        );
        let unborn = status(RepositoryHead::Unborn("main".into()), modified(0), now);
        assert_eq!(
            caption(unborn, now).accessible_label,
            "Repository status: branch main, no commits."
        );
        let clean = status(main_branch(), modified(0), now);
        assert_eq!(
            caption(clean, now).accessible_label,
            "Repository status: branch main, no changes."
        );
    }

    #[test]
    fn the_popover_lists_every_fact() {
        let now = Instant::now();
        let mut repository = status(
            main_branch(),
            ChangeState::Known(ChangeSummary {
                total: ChangeTotal::Exact(17),
                staged: 1,
                modified: 2,
                deleted: 1,
                renamed: 1,
                conflicted: 1,
                untracked: 11,
                entries: vec![
                    ChangeEntry {
                        kind: ChangeKind::Staged,
                        path: "src/ui/tab_view.rs".into(),
                    },
                    ChangeEntry {
                        kind: ChangeKind::Modified,
                        path: "src/ui/tab_manager.rs".into(),
                    },
                    ChangeEntry {
                        kind: ChangeKind::Untracked,
                        path: "notes\u{202e}.txt".into(),
                    },
                ],
            }),
            now,
        );
        repository.upstream = upstream(1, 2);
        repository.pull_request = Some(pull_request(true));
        let popover =
            RepositoryPopover::from_view(&view(repository), now + Duration::from_secs(10))
                .expect("a popover");

        assert_eq!(popover.name, "SpaceTerm");
        assert_eq!(popover.root, "/Users/sdk/Projects/SpaceTerm");
        assert_eq!(popover.branch, "main");
        assert_eq!(popover.operation, None);
        assert_eq!(
            popover.pull_request,
            Some(PopoverPullRequest {
                number: "#478".to_owned(),
                state: PullRequestState::Draft,
                title: "Show Repository Status".to_owned(),
                url: "https://github.com/sadiksaifi/spaceterm/pull/478".into(),
            })
        );
        assert_eq!(
            popover.upstream,
            Some(PopoverUpstream {
                name: "origin/main".to_owned(),
                detail: Some("↑1 ↓2".to_owned()),
            })
        );
        assert_eq!(popover.commit.as_deref(), Some("a1b2c3d"));
        let changes = popover.changes.expect("counted changes");
        assert_eq!(
            changes.summary,
            "1 conflicted, 1 staged, 1 renamed, 1 deleted, 2 modified, 11 untracked"
        );
        assert_eq!(
            changes.entries,
            vec![
                PopoverChange {
                    letter: "S",
                    kind: "Staged",
                    path: "src/ui/tab_view.rs".to_owned(),
                },
                PopoverChange {
                    letter: "M",
                    kind: "Modified",
                    path: "src/ui/tab_manager.rs".to_owned(),
                },
                PopoverChange {
                    letter: "?",
                    kind: "Untracked",
                    path: "notes\u{FFFD}.txt".to_owned(),
                },
            ]
        );
        assert_eq!(changes.more.as_deref(), Some("and 14 more"));
        assert_eq!(popover.footer, "Updated just now");
        assert!(!popover.dimmed);
    }

    #[test]
    fn the_popover_names_branch_states_and_upstream_gone() {
        let now = Instant::now();
        let popover = |repository| RepositoryPopover::from_view(&view(repository), now).unwrap();
        let mut repository = status(RepositoryHead::Detached("a1b2c3d".into()), modified(0), now);
        repository.upstream = Some(Upstream {
            name: "origin/gone".into(),
            divergence: None,
        });
        let detached = popover(repository);
        assert_eq!(detached.branch, "Detached at a1b2c3d");
        assert_eq!(
            detached.upstream,
            Some(PopoverUpstream {
                name: "origin/gone".to_owned(),
                detail: Some("Upstream gone".to_owned()),
            })
        );
        assert_eq!(detached.changes.unwrap().summary, "No changes");

        let mut unborn = status(RepositoryHead::Unborn("main".into()), modified(0), now);
        unborn.commit = None;
        let unborn = popover(unborn);
        assert_eq!(unborn.branch, "main (no commits)");
        assert_eq!(unborn.commit, None);

        let mut merging = status(main_branch(), modified(0), now);
        merging.operation = Some(RepositoryOperation::Merging);
        merging.upstream = upstream(0, 0);
        let merging = popover(merging);
        assert_eq!(merging.operation.as_deref(), Some("Merging"));
        assert_eq!(merging.upstream.unwrap().detail, None);

        let mut open = status(main_branch(), modified(0), now);
        open.pull_request = Some(pull_request(false));
        assert_eq!(
            popover(open).pull_request.unwrap().state,
            PullRequestState::Open
        );
    }

    #[test]
    fn a_truncated_count_lists_at_least_the_rest() {
        let now = Instant::now();
        let repository = status(
            main_branch(),
            ChangeState::Known(ChangeSummary {
                total: ChangeTotal::AtLeast(5_000),
                modified: 5_000,
                entries: vec![ChangeEntry {
                    kind: ChangeKind::Modified,
                    path: "a".into(),
                }],
                ..ChangeSummary::default()
            }),
            now,
        );
        let changes = RepositoryPopover::from_view(&view(repository), now)
            .unwrap()
            .changes
            .unwrap();
        assert_eq!(changes.summary, "5000+ changes");
        assert_eq!(changes.more.as_deref(), Some("and 4999+ more"));
    }

    #[test]
    fn the_popover_footer_says_how_current_the_facts_are() {
        let now = Instant::now();
        let later = now + Duration::from_secs(180);
        let footer = |repository: RepositoryStatus| {
            RepositoryPopover::from_view(&view(repository), later)
                .unwrap()
                .footer
        };
        assert_eq!(
            footer(status(main_branch(), modified(0), now)),
            "Updated 3 minutes ago"
        );
        assert_eq!(
            footer(status(main_branch(), ChangeState::Counting, now)),
            "Counting changes…"
        );
        let mut failed = status(main_branch(), modified(0), now);
        failed.freshness = Freshness::LastKnown { as_of: now };
        failed.read_failure = Some(RepositoryReadError::TimedOut);
        assert_eq!(
            footer(failed.clone()),
            "Couldn't read repository status. Last known 3 minutes ago."
        );
        failed.read_failure = None;
        assert_eq!(footer(failed.clone()), "Last known 3 minutes ago");
        let popover = RepositoryPopover::from_view(&view(failed), later).unwrap();
        assert!(popover.dimmed);
    }

    #[test]
    fn the_sidebar_shows_the_branch_or_the_pull_request() {
        let now = Instant::now();
        assert_eq!(
            SidebarBadge::from_view(&view(status(main_branch(), modified(4), now))),
            Some(SidebarBadge::Branch {
                text: "⎇ main".to_owned()
            })
        );
        assert_eq!(
            SidebarBadge::from_view(&view(status(
                RepositoryHead::Detached("a1b2c3d".into()),
                modified(0),
                now
            ))),
            Some(SidebarBadge::Branch {
                text: "◆ a1b2c3d".to_owned()
            })
        );

        let mut repository = status(main_branch(), modified(4), now);
        repository.pull_request = Some(pull_request(true));
        let Some(SidebarBadge::PullRequest {
            text,
            state,
            url,
            hover,
            accessible_label,
        }) = SidebarBadge::from_view(&view(repository))
        else {
            panic!("a Pull Request badge");
        };
        assert_eq!(text, "#478");
        assert_eq!(state, PullRequestState::Draft);
        assert_eq!(&*url, "https://github.com/sadiksaifi/spaceterm/pull/478");
        assert_eq!(
            hover.lines(),
            [
                "#478 · Draft",
                "Show Repository Status",
                "issue-441 into main"
            ]
        );
        assert_eq!(
            accessible_label,
            "Pull request 478, draft: Show Repository Status"
        );
    }

    #[test]
    fn the_chip_tooltip_gains_the_branch() {
        let now = Instant::now();
        assert_eq!(
            chip_tooltip_line(&view(status(main_branch(), modified(4), now))).as_deref(),
            Some("⎇ main")
        );
    }

    #[test]
    fn displayed_repository_strings_are_sanitized() {
        let now = Instant::now();
        let mut repository = status(
            RepositoryHead::Branch("evil\u{202e}nam\u{7}e".into()),
            modified(0),
            now,
        );
        let mut pull_request = pull_request(false);
        pull_request.title = "x".repeat(500).into();
        pull_request.head = "he\nad".into();
        repository.pull_request = Some(pull_request);
        let repository = view(repository);
        assert_eq!(
            RepositoryCaption::from_view(&repository, now)
                .unwrap()
                .branch,
            "evil\u{FFFD}nam\u{FFFD}e"
        );
        let Some(SidebarBadge::PullRequest { hover, .. }) = SidebarBadge::from_view(&repository)
        else {
            panic!("a Pull Request badge");
        };
        assert_eq!(hover.title.chars().count(), MAXIMUM_TITLE_CHARS);
        assert!(hover.title.ends_with('…'));
        assert_eq!(hover.branches, "he\u{FFFD}ad into main");
    }
}
