//! Worktree rows that leave a Workspace group, presented to the sidebar directly.

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use super::*;
use crate::repository_status::presentation::{PullRequestHover, PullRequestState, SidebarBadge};
use crate::ui::drag_and_drop::{AnimatePresence, PRESENCE_DURATION, finish_slides};

const WORKSPACE: WorkspaceId = WorkspaceId::new(1);
const PULL_REQUEST_URL: &str = "https://github.com/owner/app/pull/7";
const PULL_REQUEST_LABEL: &str = "Pull Request 7";

fn worktree(id: u64, badge: Option<SidebarBadge>) -> WorktreeRowViewModel {
    let name: SharedString = format!("app-{id}").into();
    WorktreeRowViewModel {
        worktree_id: WorktreeId::new(id),
        name: name.clone(),
        label: "main".into(),
        detached: false,
        path: format!("/srv/{name}").into(),
        directory: format!("/srv/{name}").into(),
        directory_tooltip: format!("/srv/{name}").into(),
        outside_pane_directory: None,
        repository: badge,
        main: id == 1,
        has_tabs: id == 1,
        opens_first_tab: true,
        active: id == 1,
        locked: false,
        missing: false,
        removal: WorktreeRemoval::Allowed,
        closes_tabs: false,
    }
}

fn pull_request() -> SidebarBadge {
    SidebarBadge::PullRequest {
        text: "#7".into(),
        state: PullRequestState::Open,
        url: PULL_REQUEST_URL.into(),
        hover: PullRequestHover {
            heading: "#7".into(),
            title: "Fix".into(),
            branches: "feature into main".into(),
        },
        accessible_label: PULL_REQUEST_LABEL.into(),
    }
}

/// The Workspace row, expanded over `worktrees`.
fn workspace(worktrees: Vec<WorktreeRowViewModel>) -> WorkspaceRowViewModel {
    WorkspaceRowViewModel {
        workspace_id: WORKSPACE,
        name: "Build".into(),
        path: "/srv/app".into(),
        machine: None,
        tooltip: "/srv/app".into(),
        pinned: false,
        remote_connection_phase: None,
        repository: None,
        worktrees: Some(WorktreeSection {
            expanded: true,
            repository: None,
            groups: vec![WorktreeGroup {
                former_repository: None,
                rows: worktrees,
            }],
        }),
        creates_worktrees: false,
        active: true,
        binding: None,
    }
}

fn sidebar(cx: &mut TestAppContext) -> (Entity<WorkspaceSidebar>, &mut VisualTestContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let (sidebar, cx) = cx.add_window_view(|window, cx| WorkspaceSidebar::new(true, window, cx));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (sidebar, cx)
}

fn present(
    sidebar: &Entity<WorkspaceSidebar>,
    worktrees: Vec<WorktreeRowViewModel>,
    cx: &mut VisualTestContext,
) {
    cx.update(|_, cx| {
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_rows(vec![workspace(worktrees)], Vec::new(), cx)
        });
    });
    cx.run_until_parked();
}

fn closing(worktree: u64, cx: &mut VisualTestContext) -> Option<gpui::Bounds<Pixels>> {
    cx.debug_bounds(format!("worktree-row-{}-{worktree}-closing", WORKSPACE.get()).leak())
}

fn row(worktree: u64, cx: &mut VisualTestContext) -> gpui::Bounds<Pixels> {
    ["selected", "unselected"]
        .into_iter()
        .find_map(|state| {
            cx.debug_bounds(format!("worktree-row-{}-{worktree}-{state}", WORKSPACE.get()).leak())
        })
        .unwrap_or_else(|| panic!("Worktree row {worktree} was not rendered"))
}

/// A Worktree row that leaves draws its Pull Request number as plain text while its place closes:
/// no link to click and none announced.
#[gpui::test]
fn a_leaving_worktree_row_should_draw_its_pull_request_without_a_link(cx: &mut TestAppContext) {
    let (sidebar, cx) = sidebar(cx);
    present(
        &sidebar,
        vec![worktree(1, None), worktree(2, Some(pull_request()))],
        cx,
    );
    finish_slides(cx);
    let links = |cx: &mut VisualTestContext| {
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .with_role("Link")
            .iter()
            .filter(|link| link["aria"]["label"] == PULL_REQUEST_LABEL)
            .count()
    };
    let listed = links(cx);
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    present(&sidebar, vec![worktree(1, None)], cx);
    let leaving = closing(2, cx).expect("the leaving row must draw while its place closes");
    let announced = links(cx);
    let number = leaving.bottom_right() - gpui::point(px(12.0), px(14.0));
    cx.simulate_mouse_move(number, None, Modifiers::none());
    cx.simulate_click(number, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(listed, 1, "the listed row must offer its Pull Request link");
    assert_eq!(
        (announced, cx.opened_url()),
        (0, None),
        "a leaving row must neither announce nor open its Pull Request"
    );
}

/// When the row that carries a closing gap leaves too, the row that left first keeps what showed
/// of it, so the second row keeps its place as both close.
#[gpui::test]
fn removing_the_row_below_a_closing_gap_should_keep_both_rows_where_they_draw(
    cx: &mut TestAppContext,
) {
    let (sidebar, cx) = sidebar(cx);
    let rows = |ids: &[u64]| ids.iter().map(|id| worktree(*id, None)).collect::<Vec<_>>();
    present(&sidebar, rows(&[1, 2, 3, 4]), cx);
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    present(&sidebar, rows(&[1, 3, 4]), cx);
    cx.executor().advance_clock(PRESENCE_DURATION / 4);
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
    let (first, third) = (row(1, cx), row(3, cx));
    present(&sidebar, rows(&[1, 4]), cx);
    let second = closing(2, cx).expect("the row that left first must still draw");
    let leaving = closing(3, cx).expect("the row that left second must draw");

    assert!(
        third.top() - first.bottom() < second.size.height,
        "the first gap must have closed partly for this test, got {third:?} below {first:?}"
    );
    assert_eq!(
        (second.top(), leaving.top()),
        (first.bottom(), third.top()),
        "the rows that left must keep drawing where they drew"
    );
}

/// When time passes after the last frame and the row that carries a closing gap then leaves,
/// the rows that left keep drawing where that frame drew them, so nothing jumps.
#[gpui::test]
fn removing_a_gaps_row_after_an_unpainted_moment_should_keep_the_rows_where_they_drew(
    cx: &mut TestAppContext,
) {
    let (sidebar, cx) = sidebar(cx);
    let rows = |ids: &[u64]| ids.iter().map(|id| worktree(*id, None)).collect::<Vec<_>>();
    present(&sidebar, rows(&[1, 2, 3, 4]), cx);
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    present(&sidebar, rows(&[1, 3, 4]), cx);
    cx.executor().advance_clock(PRESENCE_DURATION / 8);
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
    let (third, fourth) = (row(3, cx), row(4, cx));
    cx.executor().advance_clock(PRESENCE_DURATION / 8);
    present(&sidebar, rows(&[1, 4]), cx);
    let leaving = closing(3, cx).expect("the row that left second must draw");

    assert_eq!(
        (leaving.top(), row(4, cx).top()),
        (third.top(), fourth.top()),
        "the rows must start from where the last frame drew them"
    );
}

/// A row that returns while its old place still closes leaves that place empty, so a row that
/// left after it, already clipped away, stays hidden.
#[gpui::test]
fn a_row_returning_into_a_closing_gap_should_keep_the_rows_after_it_hidden(
    cx: &mut TestAppContext,
) {
    let (sidebar, cx) = sidebar(cx);
    let rows = |ids: &[u64]| ids.iter().map(|id| worktree(*id, None)).collect::<Vec<_>>();
    present(&sidebar, rows(&[1, 2, 3, 4]), cx);
    finish_slides(cx);
    let height = row(2, cx).size.height;
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    present(&sidebar, rows(&[1, 4]), cx);
    cx.executor().advance_clock(PRESENCE_DURATION / 4);
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
    let gap = row(4, cx).top() - row(1, cx).bottom();
    present(&sidebar, rows(&[1, 2, 4]), cx);
    let hidden = closing(3, cx);
    let fourth = row(4, cx);

    assert!(
        gap < height,
        "the gap must have closed past the first row for this test, got {gap:?}"
    );
    assert!(
        hidden.is_none_or(|third| third.top() >= fourth.top()),
        "the row that left second must stay clipped away, got {hidden:?} above {fourth:?}"
    );
}
