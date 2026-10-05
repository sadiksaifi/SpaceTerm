#![allow(dead_code, unused_imports, unused_variables)]

include!("../src/application_modules.rs");

#[cfg(target_os = "macos")]
fn main() {
    use objc2::rc::autoreleasepool;

    let dispatcher = gpui::TestDispatcher::new(0);
    let mut cx = gpui::TestAppContext::build(dispatcher.clone(), Some("native-preview-ownership"));
    autoreleasepool(|_| {
        platform::macos_quick_look::ownership_tests::preview_replacement_and_teardown_release_native_objects(&mut cx);
    });
    cx.run_until_parked();
    cx.update(|cx| {
        cx.background_executor().forbid_parking();
        cx.quit();
    });
    cx.run_until_parked();
    drop(cx);
    dispatcher.drain_tasks();
    println!("1 native Quick Look ownership test passed");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
