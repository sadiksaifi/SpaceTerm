use gpui::TestAppContext;

use super::menu_command_activates_settings;

#[gpui::test]
fn a_native_menu_command_captures_the_workspace_before_settings_activates(cx: &mut TestAppContext) {
    let notifications = std::cell::RefCell::new(Vec::new());
    let _observers = crate::platform::macos_application_menu::observe_menu_commands(
        |name, callback| notifications.borrow_mut().push((name, callback)),
        |cx: &mut gpui::App| cx.active_window(),
        crate::app::menu_command_chosen,
    );
    // Drive the production registrations through the native notification boundary. Neither
    // callback receives a window from the test; capture must query the active window itself.
    let notify = |name, cx: &mut gpui::App| {
        for (registered, callback) in notifications.borrow().iter() {
            if *registered == name {
                callback(cx);
            }
        }
    };
    menu_command_activates_settings(
        cx,
        |cx| notify("NSMenuWillSendActionNotification", cx),
        |cx| notify("NSMenuDidSendActionNotification", cx),
    );
}
