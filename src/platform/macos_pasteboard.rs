use cocoa::appkit::{NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeString};
use cocoa::base::{YES, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSInteger, NSString};
use objc::{msg_send, sel, sel_impl};
#[cfg(test)]
use std::ffi::CStr;
use std::path::PathBuf;

use crate::terminal::Osc52Target;
use crate::terminal::osc52::{Osc52Clipboard, Osc52ClipboardError};

#[cfg(test)]
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

pub(crate) struct MacosFileClipboard;
impl FileClipboard for MacosFileClipboard {
    fn read_files(&self) -> Result<Vec<PathBuf>, ClipboardError> {
        read_file_urls().map_err(|_| ClipboardError::InvalidFiles)
    }
}

pub(crate) fn read_file_urls() -> Result<Vec<PathBuf>, String> {
    // SAFETY: values are copied from the general pasteboard during this synchronous AppKit call.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        let result = read_file_urls_from_pasteboard(pasteboard);
        pool.drain();
        result
    }
}

fn read_file_urls_from_pasteboard(pasteboard: cocoa::base::id) -> Result<Vec<PathBuf>, String> {
    // SAFETY: The caller owns the pasteboard and an autorelease pool for this synchronous read.
    unsafe {
        let items: cocoa::base::id = msg_send![pasteboard, pasteboardItems];
        let count: usize = msg_send![items, count];
        let file_url_type = NSString::alloc(nil)
            .init_str("public.file-url")
            .autorelease();
        if count > MAX_FILE_ITEMS {
            return Err("too many clipboard items".to_owned());
        }
        let mut urls = Vec::new();
        let mut bytes = 0usize;
        for index in 0..count {
            let item: cocoa::base::id = msg_send![items, objectAtIndex: index];
            let value: cocoa::base::id = msg_send![item, stringForType: file_url_type];
            if value == nil {
                continue;
            }
            let length: usize = msg_send![value, lengthOfBytesUsingEncoding: 4_usize];
            if length > MAX_FILE_INSERTION_BYTES.saturating_sub(bytes) {
                return Err("clipboard files exceed the size limit".to_owned());
            }
            bytes += length;
            let utf8: *const std::os::raw::c_char = msg_send![value, UTF8String];
            if utf8.is_null() {
                return Err("file URL is not valid UTF-8".to_owned());
            }
            let raw = std::slice::from_raw_parts(utf8.cast::<u8>(), length);
            let text =
                std::str::from_utf8(raw).map_err(|_| "file URL is not valid UTF-8".to_owned())?;
            urls.push(text.to_owned());
        }
        parse_file_urls(&urls).map_err(str::to_owned)
    }
}

pub(crate) fn write_selection(plain_text: &str, html: Option<&str>) -> Result<(), String> {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        let result = write_selection_to_pasteboard(pasteboard, plain_text, html);
        pool.drain();
        result
    }
}

fn write_selection_to_pasteboard(
    pasteboard: cocoa::base::id,
    plain_text: &str,
    html: Option<&str>,
) -> Result<(), String> {
    let representations = selection_representations(plain_text, html)
        .into_iter()
        .map(|representation| {
            let pasteboard_type = match representation.mime {
                PLAIN_TEXT_MIME => unsafe { NSPasteboardTypeString },
                HTML_MIME => unsafe { NSPasteboardTypeHTML },
                _ => unreachable!("selection pasteboard MIME types are closed"),
            };
            (representation, pasteboard_type)
        })
        .collect::<Vec<_>>();
    let types = representations
        .iter()
        .map(|(_, pasteboard_type)| *pasteboard_type)
        .collect::<Vec<_>>();

    unsafe {
        let types = NSArray::arrayWithObjects(nil, &types);
        let _: NSInteger = pasteboard.declareTypes_owner(types, nil);
        for (representation, pasteboard_type) in representations {
            let value = NSString::alloc(nil)
                .init_str(representation.text)
                .autorelease();
            if pasteboard.setString_forType(value, pasteboard_type) != YES {
                return Err(format!(
                    "macOS refused terminal selection representation {}",
                    representation.mime
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(crate) struct MacosOsc52Clipboard;

pub(crate) struct MacosOsc52ClipboardFactory;
impl crate::terminal::osc52::Osc52ClipboardFactory for MacosOsc52ClipboardFactory {
    fn create(&self) -> Box<dyn Osc52Clipboard> {
        Box::new(MacosOsc52Clipboard)
    }
}

impl Osc52Clipboard for MacosOsc52Clipboard {
    fn read(&mut self, target: Osc52Target) -> Result<String, Osc52ClipboardError> {
        if target != Osc52Target::Standard {
            return Err(Osc52ClipboardError::UnsupportedTarget);
        }
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let value = NSPasteboard::generalPasteboard(nil).stringForType(NSPasteboardTypeString);
            let result = if value == nil {
                Ok(String::new())
            } else {
                let length: usize = msg_send![value, lengthOfBytesUsingEncoding: 4_usize];
                if length > crate::terminal::osc52::MAX_OSC52_CONTENT_BYTES {
                    pool.drain();
                    return Err(Osc52ClipboardError::Unavailable);
                }
                let pointer = value.UTF8String();
                if pointer.is_null() {
                    Err(Osc52ClipboardError::Unavailable)
                } else {
                    std::str::from_utf8(std::slice::from_raw_parts(pointer.cast::<u8>(), length))
                        .map(str::to_owned)
                        .map_err(|_| Osc52ClipboardError::Unavailable)
                }
            };
            pool.drain();
            result
        }
    }

    fn write(&mut self, target: Osc52Target, text: &str) -> Result<(), Osc52ClipboardError> {
        if target != Osc52Target::Standard {
            return Err(Osc52ClipboardError::UnsupportedTarget);
        }
        write_selection(text, None).map_err(|_| Osc52ClipboardError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_file_discovery_preserves_items_and_rejects_invalid_authority() {
        use cocoa::base::id;
        use objc::class;
        // SAFETY: This test owns an isolated pasteboard and balances all retained objects.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let pasteboard = NSPasteboard::pasteboardWithUniqueName(nil);
            let file_type = NSString::alloc(nil)
                .init_str("public.file-url")
                .autorelease();
            let first: id = msg_send![class!(NSPasteboardItem), new];
            let second: id = msg_send![class!(NSPasteboardItem), new];
            let a = NSString::alloc(nil).init_str("file:///a%20b").autorelease();
            let b = NSString::alloc(nil).init_str("file:///c").autorelease();
            let _: bool = msg_send![first, setString: a forType: file_type];
            let _: bool = msg_send![second, setString: b forType: file_type];
            let items = NSArray::arrayWithObjects(nil, &[first, second]);
            let _: NSInteger = msg_send![pasteboard, clearContents];
            let _: bool = msg_send![pasteboard, writeObjects: items];
            let paths = read_file_urls_from_pasteboard(pasteboard).unwrap();
            assert!(paths == vec![PathBuf::from("/a b"), PathBuf::from("/c")]);
            let remote = NSString::alloc(nil)
                .init_str("file://remote/a")
                .autorelease();
            let _: NSInteger = msg_send![pasteboard, clearContents];
            let _: bool = msg_send![pasteboard, setString: remote forType: file_type];
            assert!(read_file_urls_from_pasteboard(pasteboard).is_err());
            let _: () = msg_send![first, release];
            let _: () = msg_send![second, release];
            pasteboard.releaseGlobally();
            pool.drain();
        }
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
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let pasteboard = NSPasteboard::pasteboardWithUniqueName(nil);

            write_selection_to_pasteboard(
                pasteboard,
                "native selection",
                Some("<pre>native selection</pre>"),
            )
            .unwrap();

            let types = pasteboard.types();
            let has_plain_text: bool = msg_send![types, containsObject: NSPasteboardTypeString];
            let has_html: bool = msg_send![types, containsObject: NSPasteboardTypeHTML];
            let plain_text = pasteboard.stringForType(NSPasteboardTypeString);
            let plain_text = CStr::from_ptr(NSString::UTF8String(plain_text))
                .to_string_lossy()
                .into_owned();
            pasteboard.releaseGlobally();
            pool.drain();

            assert_eq!(
                (has_plain_text, has_html, plain_text),
                (true, true, "native selection".to_owned())
            );
        }
    }
}
