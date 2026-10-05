use objc2::MainThreadMarker;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeHTML, NSPasteboardTypeString};
use objc2_foundation::{NSArray, NSString, NSURL, NSUTF8StringEncoding};
use std::path::PathBuf;

#[cfg(all(test, feature = "native-tests"))]
use crate::terminal::native_services::clipboard::PasteboardRepresentation;
use crate::terminal::native_services::clipboard::{
    ClipboardError, ClipboardRead, FileClipboard, HTML_MIME, PLAIN_TEXT_MIME, SelectionClipboard,
    TextClipboard, TextClipboardTarget, selection_representations,
};
use crate::terminal::native_services::file_insertion::{
    MAX_FILE_INSERTION_BYTES, MAX_FILE_ITEMS, parse_file_urls,
};
use crate::terminal::osc52::{MAX_OSC52_CONTENT_BYTES, Osc52Target};

pub(crate) struct MacosTextClipboard;
impl TextClipboard for MacosTextClipboard {
    fn resolve(&self, target: Osc52Target) -> TextClipboardTarget {
        match target {
            Osc52Target::Default
            | Osc52Target::Standard
            | Osc52Target::Primary
            | Osc52Target::Selection => TextClipboardTarget::Clipboard,
        }
    }

    /// The general pasteboard answers on the main thread, so the read completes at once.
    fn read(
        &self,
        target: TextClipboardTarget,
        _: &mut gpui::App,
    ) -> ClipboardRead<Option<String>> {
        Box::pin(std::future::ready(read_general_text(target)))
    }

    fn write(
        &self,
        target: TextClipboardTarget,
        text: &str,
        _: &mut gpui::App,
    ) -> Result<(), ClipboardError> {
        if target != TextClipboardTarget::Clipboard {
            return Err(ClipboardError::Unavailable);
        }
        write_selection(text, None).map_err(|_| ClipboardError::Unavailable)
    }
}

fn read_general_text(target: TextClipboardTarget) -> Result<Option<String>, ClipboardError> {
    if target != TextClipboardTarget::Clipboard {
        return Err(ClipboardError::Unavailable);
    }
    MainThreadMarker::new().ok_or(ClipboardError::Unavailable)?;
    read_text_from_pasteboard(&NSPasteboard::generalPasteboard())
}

fn read_text_from_pasteboard(pasteboard: &NSPasteboard) -> Result<Option<String>, ClipboardError> {
    // SAFETY: AppKit exports this immutable pasteboard type constant.
    let text_type = unsafe { NSPasteboardTypeString };
    if !pasteboard
        .types()
        .is_some_and(|types| types.containsObject(text_type))
    {
        return Ok(None);
    }
    let data = pasteboard
        .dataForType(text_type)
        .ok_or(ClipboardError::Unavailable)?;
    if data.len() > MAX_OSC52_CONTENT_BYTES {
        return Err(ClipboardError::InvalidText);
    }
    // SAFETY: The retained pasteboard data is immutable during this synchronous read.
    let bytes = unsafe { data.as_bytes_unchecked() };
    std::str::from_utf8(bytes)
        .map(|text| Some(text.to_owned()))
        .map_err(|_| ClipboardError::InvalidText)
}

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
    /// The general pasteboard answers on the main thread, so the read completes at once.
    fn read_files(&self, _: &mut gpui::App) -> ClipboardRead<Vec<PathBuf>> {
        Box::pin(std::future::ready(read_file_urls(self.paths)))
    }
}

pub(crate) fn read_file_urls(
    paths: crate::local_path::LocalPathSemantics,
) -> Result<Vec<PathBuf>, ClipboardError> {
    MainThreadMarker::new().ok_or(ClipboardError::Unavailable)?;
    read_file_urls_from_pasteboard(&NSPasteboard::generalPasteboard(), paths)
}

fn read_file_urls_from_pasteboard(
    pasteboard: &NSPasteboard,
    paths: crate::local_path::LocalPathSemantics,
) -> Result<Vec<PathBuf>, ClipboardError> {
    let Some(items) = pasteboard.pasteboardItems() else {
        return Ok(Vec::new());
    };
    read_file_urls_from_items(&items, paths).map_err(|_| ClipboardError::InvalidFiles)
}

fn read_file_urls_from_items(
    items: &NSArray<NSPasteboardItem>,
    paths: crate::local_path::LocalPathSemantics,
) -> Result<Vec<PathBuf>, String> {
    let file_url_type = NSString::from_str("public.file-url");
    let mut urls = Vec::new();
    let mut source_bytes = 0usize;
    let mut resolved_bytes = 0usize;
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
        let source = read_file_url_text(
            &value,
            MAX_FILE_INSERTION_BYTES.saturating_sub(source_bytes),
            copy_file_url_utf8,
        )?;
        source_bytes += source.len();
        parse_file_urls(paths, std::slice::from_ref(&source)).map_err(str::to_owned)?;
        let url =
            NSURL::URLWithString(&value).ok_or_else(|| "file URL is unreadable".to_owned())?;
        let path_url = url
            .filePathURL()
            .filter(|url| !url.isFileReferenceURL())
            .ok_or_else(|| "file URL cannot be resolved".to_owned())?;
        let path_url_text = path_url
            .absoluteString()
            .ok_or_else(|| "file URL is unreadable".to_owned())?;
        let resolved = read_file_url_text(
            &path_url_text,
            MAX_FILE_INSERTION_BYTES.saturating_sub(resolved_bytes),
            copy_file_url_utf8,
        )?;
        resolved_bytes += resolved.len();
        urls.push(resolved);
    }
    parse_file_urls(paths, &urls).map_err(str::to_owned)
}

fn read_file_url_text(
    value: &NSString,
    remaining: usize,
    copy: impl FnOnce(&NSString, usize) -> Result<String, String>,
) -> Result<String, String> {
    let byte_len = value.lengthOfBytesUsingEncoding(NSUTF8StringEncoding);
    if byte_len > remaining {
        return Err("clipboard files exceed the size limit".to_owned());
    }
    copy(value, byte_len)
}

fn copy_file_url_utf8(value: &NSString, byte_len: usize) -> Result<String, String> {
    let utf8 = value.UTF8String();
    if utf8.is_null() {
        return Err("file URL is unreadable".to_owned());
    }
    // SAFETY: NSString retains the UTF-8 buffer through this synchronous copy. The byte length
    // was checked against the remaining Paste Payload limit before constructing the slice.
    let bytes = unsafe { std::slice::from_raw_parts(utf8.cast::<u8>(), byte_len) };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| "file URL is unreadable".to_owned())
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

#[cfg(all(test, feature = "native-tests"))]
#[allow(dead_code)]
pub(in crate::platform) mod tests {
    use super::*;
    use objc2::runtime::ProtocolObject;
    use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSPasteboardItemDataProvider, NSPasteboardType, NSPasteboardTypePNG, NSPasteboardWriting,
    };
    use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSURL};
    use std::cell::Cell;

    #[test]
    fn macos_osc52_selectors_keep_system_clipboard_aliases() {
        for target in [
            Osc52Target::Default,
            Osc52Target::Standard,
            Osc52Target::Primary,
            Osc52Target::Selection,
        ] {
            assert_eq!(
                MacosTextClipboard.resolve(target),
                TextClipboardTarget::Clipboard
            );
        }
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; the provider owns its read counter.
        #[unsafe(super(NSObject))]
        #[name = "SpaceTermClipboardTestImageProvider"]
        #[thread_kind = MainThreadOnly]
        #[ivars = Cell<usize>]
        struct ImageProvider;

        unsafe impl NSObjectProtocol for ImageProvider {}

        unsafe impl NSPasteboardItemDataProvider for ImageProvider {
            #[unsafe(method(pasteboard:item:provideDataForType:))]
            fn provide_data(
                &self,
                _: Option<&NSPasteboard>,
                item: &NSPasteboardItem,
                ty: &NSPasteboardType,
            ) {
                self.ivars().set(self.ivars().get() + 1);
                let _ = item.setData_forType(&NSData::with_bytes(b"image fixture"), ty);
            }
        }
    );

    pub(in crate::platform) fn native_text_clipboard_ignores_image_representations() {
        let mtm = MainThreadMarker::new().unwrap();
        let allocated = ImageProvider::alloc(mtm).set_ivars(Cell::new(0));
        // SAFETY: NSObject's init is the designated initializer for this provider.
        let provider: objc2::rc::Retained<ImageProvider> =
            unsafe { msg_send![super(allocated), init] };
        let pasteboard = PrivatePasteboard(NSPasteboard::pasteboardWithUniqueName());
        let item = NSPasteboardItem::new();
        // SAFETY: AppKit exports this immutable pasteboard type constant.
        let image_type = unsafe { NSPasteboardTypePNG };
        assert!(item.setDataProvider_forTypes(
            ProtocolObject::from_ref(&*provider),
            &NSArray::from_slice(&[image_type]),
        ));
        assert!(
            pasteboard
                .0
                .writeObjects(&NSArray::from_slice(&[ProtocolObject::<
                    dyn NSPasteboardWriting,
                >::from_ref(&*item),]))
        );
        assert_eq!(read_text_from_pasteboard(&pasteboard.0), Ok(None));
        assert_eq!(provider.ivars().get(), 0);
        // The fixture must detect an actual image request.
        assert!(pasteboard.0.dataForType(image_type).is_some());
        assert_eq!(provider.ivars().get(), 1);
    }

    pub(in crate::platform) fn native_text_clipboard_preserves_utf8_and_rejects_oversized_text() {
        let pasteboard = PrivatePasteboard(NSPasteboard::pasteboardWithUniqueName());
        let boundary = "😀".repeat(crate::terminal::osc52::MAX_OSC52_CONTENT_BYTES / 4);
        for text in ["", "a\0😀\ntext", boundary.as_str()] {
            write_selection_to_pasteboard(&pasteboard.0, text, None).unwrap();
            assert_eq!(
                read_text_from_pasteboard(&pasteboard.0).unwrap().as_deref(),
                Some(text)
            );
        }
        write_selection_to_pasteboard(&pasteboard.0, &(boundary + "x"), None).unwrap();
        assert!(matches!(
            read_text_from_pasteboard(&pasteboard.0),
            Err(ClipboardError::InvalidText)
        ));
        // SAFETY: AppKit exports this immutable pasteboard type constant.
        let text_type = unsafe { NSPasteboardTypeString };
        assert!(
            pasteboard
                .0
                .setData_forType(Some(&NSData::with_bytes(&[0xff])), text_type)
        );
        assert_eq!(
            read_text_from_pasteboard(&pasteboard.0),
            Err(ClipboardError::InvalidText)
        );
    }

    struct FileReferenceFixture {
        path: PathBuf,
    }

    impl FileReferenceFixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "spaceterm-file-reference-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join("a b.txt");
            std::fs::write(&path, b"file reference fixture").unwrap();
            Self { path }
        }

        fn reference_url(&self) -> String {
            let path = NSString::from_str(self.path.to_str().unwrap());
            let url = NSURL::fileURLWithPath(&path);
            let reference = url.fileReferenceURL().unwrap();
            assert!(reference.isFileReferenceURL());
            reference.absoluteString().unwrap().to_string()
        }
    }

    impl Drop for FileReferenceFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir(self.path.parent().unwrap());
        }
    }

    struct PrivatePasteboard(objc2::rc::Retained<NSPasteboard>);

    impl PrivatePasteboard {
        fn with_file_urls(urls: &[&str]) -> Self {
            let pasteboard = NSPasteboard::pasteboardWithUniqueName();
            let file_type = file_type();
            let items = urls
                .iter()
                .map(|url| item(url, &file_type))
                .collect::<Vec<_>>();
            let objects = items
                .iter()
                .map(|item| ProtocolObject::<dyn NSPasteboardWriting>::from_ref(&**item))
                .collect::<Vec<_>>();
            assert!(pasteboard.writeObjects(&NSArray::from_slice(&objects)));
            Self(pasteboard)
        }
    }

    impl Drop for PrivatePasteboard {
        fn drop(&mut self) {
            // SAFETY: This fixture exclusively owns its named server-side pasteboard.
            let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
        }
    }

    fn file_type() -> objc2::rc::Retained<NSString> {
        NSString::from_str("public.file-url")
    }

    fn item(value: &str, ty: &NSPasteboardType) -> objc2::rc::Retained<NSPasteboardItem> {
        let item = NSPasteboardItem::new();
        assert!(item.setString_forType(&NSString::from_str(value), ty));
        item
    }

    pub(in crate::platform) fn oversized_file_url_is_rejected_before_conversion() {
        let value = NSString::from_str("file:///large");
        let result = read_file_url_text(&value, 1, |_, _| {
            panic!("oversized file URL reached conversion")
        });
        assert_eq!(result.unwrap_err(), "clipboard files exceed the size limit");
    }

    pub(in crate::platform) fn native_file_discovery_counts_only_file_representations() {
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

    pub(in crate::platform) fn native_file_discovery_rejects_unreadable_file_representation_with_text()
     {
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

    pub(in crate::platform) fn native_file_discovery_preserves_items_and_rejects_invalid_authority()
    {
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

    pub(in crate::platform) fn native_file_reference_url_inserts_resolved_path() {
        let file = FileReferenceFixture::new();
        let reference = file.reference_url();
        let pasteboard = PrivatePasteboard::with_file_urls(&[&reference]);

        let paths = read_file_urls_from_pasteboard(
            &pasteboard.0,
            crate::local_path::LocalPathSemantics::Posix,
        )
        .unwrap();
        let insertion = crate::terminal::native_services::file_insertion::prepare_file_insertion(
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
            &paths,
        )
        .unwrap();

        assert_eq!(insertion.text, format!("'{}'", file.path.display()));
    }

    pub(in crate::platform) fn native_deleted_file_reference_url_returns_typed_failure() {
        let file = FileReferenceFixture::new();
        let reference = file.reference_url();
        let pasteboard = PrivatePasteboard::with_file_urls(&[&reference]);
        std::fs::remove_file(&file.path).unwrap();

        let result = read_file_urls_from_pasteboard(
            &pasteboard.0,
            crate::local_path::LocalPathSemantics::Posix,
        );

        assert_eq!(result, Err(ClipboardError::InvalidFiles));
    }

    pub(in crate::platform) fn native_file_reference_and_path_urls_preserve_order() {
        let file = FileReferenceFixture::new();
        let reference = file.reference_url();
        let pasteboard = PrivatePasteboard::with_file_urls(&[&reference, "file:///plain%20path"]);

        let paths = read_file_urls_from_pasteboard(
            &pasteboard.0,
            crate::local_path::LocalPathSemantics::Posix,
        )
        .unwrap();

        assert_eq!(paths, vec![file.path.clone(), PathBuf::from("/plain path")]);
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

    pub(in crate::platform) fn native_write_declares_every_representation_before_publishing_data() {
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
