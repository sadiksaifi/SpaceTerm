#[cfg(target_os = "macos")]
#[path = "../src/platform/macos_quick_look_window.rs"]
mod macos_quick_look_window;

#[cfg(target_os = "macos")]
fn main() {
    use macos_quick_look_window::OwnedQuickLookWindow;
    use objc2::MainThreadMarker;
    use objc2::rc::{Weak, autoreleasepool};

    let mtm = MainThreadMarker::new().expect("native ownership test must run on the main thread");
    let (first_panel, first_preview, second_panel, second_preview, current) =
        autoreleasepool(|_| {
            let first = OwnedQuickLookWindow::new(mtm).expect("first preview should initialize");
            let first_panel = Weak::from_retained(&first.panel);
            let first_preview = Weak::from_retained(&first.preview);
            let second = OwnedQuickLookWindow::new(mtm).expect("second preview should initialize");
            let second_panel = Weak::from_retained(&second.panel);
            let second_preview = Weak::from_retained(&second.preview);
            let mut current = Some(first);
            drop(current.replace(second));
            (
                first_panel,
                first_preview,
                second_panel,
                second_preview,
                current,
            )
        });
    assert!(first_panel.load().is_none());
    assert!(first_preview.load().is_none());
    assert!(second_panel.load().is_some());
    assert!(second_preview.load().is_some());
    autoreleasepool(|_| drop(current));
    assert!(second_panel.load().is_none());
    assert!(second_preview.load().is_none());
    println!("1 native Quick Look ownership test passed");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
