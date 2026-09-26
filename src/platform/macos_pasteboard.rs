use objc2::MainThreadMarker;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeHTML, NSPasteboardTypeString};
use objc2_foundation::{NSArray, NSString};
use std::path::PathBuf;

#[cfg(all(test, feature = "macos-native-tests"))]
use crate::terminal::native_services::clipboard::PasteboardRepresentation;
use crate::terminal::native_services::clipboard::{
    ClipboardError, FileClipboard, HTML_MIME, PLAIN_TEXT_MIME, SelectionClipboard,
    selection_representations,
};
use crate::terminal::native_services::file_insertion::{
    MAX_FILE_INSERTION_BYTES, MAX_FILE_ITEMS, parse_file_urls,
};

pub(crate) struct MacosSelectionClipboard;
impl SelectionClipboard for MacosSelectionClipboard {
    fn publish(
        &self,
        copy: &crate::terminal::SelectionCopy,
        _: &mut gpui::App,
    ) -> Result<(), ClipboardError> {
        write_selection(&copy.plain_text, copy.html.as_deref())
            .map_err(|_| ClipboardError::Unavailable)
    }
}

pub(crate) struct MacosFileClipboard {
    pub(crate) paths: crate::local_path::LocalPathSemantics,
}
impl FileClipboard for MacosFileClipboard {
    fn read_files(&self) -> Result<Vec<PathBuf>, ClipboardError> {
        read_file_urls(self.paths).map_err(|_| ClipboardError::InvalidFiles)
    }
}

pub(crate) fn read_file_urls(
    paths: crate::local_path::LocalPathSemantics,
) -> Result<Vec<PathBuf>, String> {
    MainThreadMarker::new().ok_or_else(|| "pasteboard unavailable".to_owned())?;
    read_file_urls_from_pasteboard(&NSPasteboard::generalPasteboard(), paths)
}

fn read_file_urls_from_pasteboard(
    pasteboard: &NSPasteboard,
    paths: crate::local_path::LocalPathSemantics,
) -> Result<Vec<PathBuf>, String> {
    let Some(items) = pasteboard.pasteboardItems() else {
        return Ok(Vec::new());
    };
    read_file_urls_from_items(&items, paths)
}

fn read_file_urls_from_items(
    items: &NSArray<NSPasteboardItem>,
    paths: crate::local_path::LocalPathSemantics,
) -> Result<Vec<PathBuf>, String> {
    let file_url_type = NSString::from_str("public.file-url");
    let mut urls = Vec::new();
    let mut bytes = 0usize;
    for index in 0..items.count() {
        let item = items.objectAtIndex(index);
        if !item.types().containsObject(&file_url_type) {
            continue;
        }
        if urls.len() >= MAX_FILE_ITEMS {
            return Err("too many clipboard files".to_owned());
        }
        let value = item
            .stringForType(&file_url_type)
            .ok_or_else(|| "file URL is unreadable".to_owned())?;
        let text = value.to_string();
        if text.len() > MAX_FILE_INSERTION_BYTES.saturating_sub(bytes) {
            return Err("clipboard files exceed the size limit".to_owned());
        }
        bytes += text.len();
        urls.push(text);
    }
    parse_file_urls(paths, &urls).map_err(str::to_owned)
}

pub(crate) fn write_selection(plain_text: &str, html: Option<&str>) -> Result<(), String> {
    MainThreadMarker::new().ok_or_else(|| "pasteboard unavailable".to_owned())?;
    write_selection_to_pasteboard(&NSPasteboard::generalPasteboard(), plain_text, html)
}

fn write_selection_to_pasteboard(
    pasteboard: &NSPasteboard,
    plain_text: &str,
    html: Option<&str>,
) -> Result<(), String> {
    let representations = selection_representations(plain_text, html)
        .into_iter()
        .map(|representation| {
            let pasteboard_type = match representation.mime {
                // SAFETY: AppKit exports these immutable pasteboard type constants.
                PLAIN_TEXT_MIME => unsafe { NSPasteboardTypeString },
                // SAFETY: AppKit exports these immutable pasteboard type constants.
                HTML_MIME => unsafe { NSPasteboardTypeHTML },
                _ => unreachable!("selection pasteboard MIME types are closed"),
            };
            (representation, pasteboard_type)
        })
        .collect::<Vec<_>>();
    let types = NSArray::from_slice(
        &representations
            .iter()
            .map(|(_, ty)| *ty)
            .collect::<Vec<_>>(),
    );
    // SAFETY: A nil owner needs no NSPasteboardOwner protocol implementation.
    unsafe { pasteboard.declareTypes_owner(&types, None) };
    for (representation, pasteboard_type) in representations {
        if !pasteboard.setString_forType(&NSString::from_str(representation.text), pasteboard_type)
        {
            return Err(format!(
                "macOS refused terminal selection representation {}",
                representation.mime
            ));
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;
    use objc2::msg_send;
    use objc2_app_kit::NSPasteboardType;
    use objc2_foundation::NSData;

    fn file_type() -> objc2::rc::Retained<NSString> {
        NSString::from_str("public.file-url")
    }

    fn item(value: &str, ty: &NSPasteboardType) -> objc2::rc::Retained<NSPasteboardItem> {
        let item = NSPasteboardItem::new();
        assert!(item.setString_forType(&NSString::from_str(value), ty));
        item
    }

    #[test]
    fn native_file_discovery_counts_only_file_representations() {
        let file_type = file_type();
        // SAFETY: AppKit exports this immutable pasteboard type constant.
        let text_type = unsafe { NSPasteboardTypeString };
        for (text_count, file_count) in [
            (MAX_FILE_ITEMS + 1, 0),
            (1, MAX_FILE_ITEMS),
            (0, MAX_FILE_ITEMS + 1),
        ] {
            let mut items = Vec::new();
            for index in 0..text_count + file_count {
                items.push(if index < text_count {
                    item("ordinary text", text_type)
                } else {
                    item("file:///a", &file_type)
                });
            }
            let items = NSArray::from_retained_slice(&items);
            let result =
                read_file_urls_from_items(&items, crate::local_path::LocalPathSemantics::Posix);
            if file_count > MAX_FILE_ITEMS {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), vec![PathBuf::from("/a"); file_count]);
            }
        }
    }

    #[test]
    fn native_file_discovery_rejects_unreadable_file_representation_with_text() {
        let file_type = file_type();
        let item = NSPasteboardItem::new();
        assert!(item.setData_forType(&NSData::with_bytes(&[0xff]), &file_type));
        // SAFETY: AppKit exports this immutable pasteboard type constant.
        assert!(
            item.setString_forType(&NSString::from_str("alternate text"), unsafe {
                NSPasteboardTypeString
            })
        );
        let items = NSArray::from_retained_slice(&[item]);
        assert!(
            read_file_urls_from_items(&items, crate::local_path::LocalPathSemantics::Posix)
                .is_err()
        );
    }

    #[test]
    fn native_file_discovery_preserves_items_and_rejects_invalid_authority() {
        let file_type = file_type();
        let items = NSArray::from_retained_slice(&[
            item("file:///a%20b", &file_type),
            item("file:///c", &file_type),
        ]);
        let paths = read_file_urls_from_items(&items, crate::local_path::LocalPathSemantics::Posix)
            .unwrap();
        assert_eq!(paths, vec![PathBuf::from("/a b"), PathBuf::from("/c")]);
        let remote_items = NSArray::from_retained_slice(&[item("file://remote/a", &file_type)]);
        assert!(
            read_file_urls_from_items(&remote_items, crate::local_path::LocalPathSemantics::Posix)
                .is_err()
        );
    }

    #[test]
    fn selection_copy_converts_only_to_public_text_mime_representations() {
        let representations =
            selection_representations("alpha & beta", Some("<pre>alpha &amp; beta</pre>"));

        assert_eq!(
            representations,
            vec![
                PasteboardRepresentation {
                    mime: PLAIN_TEXT_MIME,
                    text: "alpha & beta",
                },
                PasteboardRepresentation {
                    mime: HTML_MIME,
                    text: "<pre>alpha &amp; beta</pre>",
                },
            ]
        );
        assert!(representations.iter().all(|representation| {
            !representation.text.contains("CellSnapshot")
                && !representation.text.contains("SelectionCopy")
        }));
    }

    #[test]
    fn absent_rich_text_publishes_only_plain_text() {
        assert_eq!(
            selection_representations("plain", None),
            vec![PasteboardRepresentation {
                mime: PLAIN_TEXT_MIME,
                text: "plain",
            }]
        );
    }

    #[test]
    fn native_write_declares_every_representation_before_publishing_data() {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        write_selection_to_pasteboard(
            &pasteboard,
            "native selection",
            Some("<pre>native selection</pre>"),
        )
        .unwrap();
        let types = pasteboard.types().unwrap();
        // SAFETY: AppKit exports these immutable pasteboard type constants.
        let (plain_type, html_type) = unsafe { (NSPasteboardTypeString, NSPasteboardTypeHTML) };
        let has_plain_text = types.containsObject(plain_type);
        let has_html = types.containsObject(html_type);
        let plain_text = pasteboard.stringForType(plain_type).unwrap().to_string();
        // SAFETY: This unique test pasteboard supports releaseGlobally and no other code holds it.
        let _: () = unsafe { msg_send![&*pasteboard, releaseGlobally] };
        assert_eq!(
            (has_plain_text, has_html, plain_text),
            (true, true, "native selection".to_owned())
        );
    }
}
