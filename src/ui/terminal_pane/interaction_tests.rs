use super::*;

#[gpui::test]
fn server_backed_repository_status_keeps_its_command_handler(cx: &mut TestAppContext) {
    let (pane, cx, _) = server_terminal_pane(cx, true);
    open_crowded_repository_popover(&pane, cx);
    assert!(cx.debug_bounds("repository-status-popover").is_some());
    assert!(pane.read_with(cx, |pane, _| pane.repository_popover_open()));
}
