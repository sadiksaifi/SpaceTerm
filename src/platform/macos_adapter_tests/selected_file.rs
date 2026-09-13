use crate::platform::macos_selected_file::MacosSelectedFileOpener;
use crate::platform::selected_file::SelectedFileOpener;

#[test]
fn a_selected_fifo_opens_without_waiting_for_a_writer() {
    let path = std::env::temp_dir().join(format!("spaceterm-selected-fifo-{}", std::process::id()));
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: name is a live NUL-terminated path for this test's private FIFO.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (sender, receiver) = std::sync::mpsc::channel();
    let selected_path = path.clone();
    let worker = std::thread::spawn(move || {
        let result = MacosSelectedFileOpener
            .open(&selected_path)
            .map(|file| file.metadata().expect("selected object metadata").is_file());
        let _ = sender.send(result);
    });
    let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
    std::fs::remove_file(path).expect("remove FIFO");

    assert_eq!(result.expect("opening must not block"), Ok(false));
    worker.join().expect("opener thread");
}
