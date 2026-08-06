#[cfg(not(test))]
use cocoa::appkit::{NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeString};
#[cfg(not(test))]
use cocoa::base::{YES, nil};
#[cfg(not(test))]
use cocoa::foundation::{NSAutoreleasePool, NSInteger, NSString};
#[cfg(not(test))]
use objc::{class, msg_send, sel, sel_impl};
#[cfg(not(test))]
use std::ffi::CStr;
use std::path::PathBuf;

#[cfg(not(test))]
use crate::terminal::Osc52Target;
#[cfg(not(test))]
use crate::terminal::osc52::{Osc52Clipboard, Osc52ClipboardError};

pub(crate) const PLAIN_TEXT_MIME: &str = "text/plain;charset=utf-8";
pub(crate) const HTML_MIME: &str = "text/html;charset=utf-8";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PasteboardRepresentation<'a> {
    pub(crate) mime: &'static str,
    pub(crate) text: &'a str,
}

pub(crate) fn selection_representations<'a>(
    plain_text: &'a str,
    html: Option<&'a str>,
) -> Vec<PasteboardRepresentation<'a>> {
    let mut representations = vec![PasteboardRepresentation {
        mime: PLAIN_TEXT_MIME,
        text: plain_text,
    }];
    if let Some(html) = html.filter(|html| !html.is_empty()) {
        representations.push(PasteboardRepresentation {
            mime: HTML_MIME,
            text: html,
        });
    }
    representations
}

#[cfg(not(test))]
pub(crate) fn read_file_urls() -> Result<Vec<PathBuf>, String> {
    // SAFETY: values are copied from the general pasteboard during this synchronous AppKit call.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        let items: cocoa::base::id = msg_send![pasteboard, pasteboardItems];
        let count: usize = msg_send![items, count];
        let file_url_type = NSString::alloc(nil)
            .init_str("public.file-url")
            .autorelease();
        let mut paths = Vec::new();
        for index in 0..count {
            let item: cocoa::base::id = msg_send![items, objectAtIndex: index];
            let value: cocoa::base::id = msg_send![item, stringForType: file_url_type];
            if value == nil {
                continue;
            }
            let url: cocoa::base::id = msg_send![class!(NSURL), URLWithString: value];
            let is_file: bool = msg_send![url, isFileURL];
            if !is_file {
                pool.drain();
                return Err("pasteboard URL is not a file URL".to_owned());
            }
            let path: cocoa::base::id = msg_send![url, path];
            let utf8: *const std::os::raw::c_char = msg_send![path, UTF8String];
            if utf8.is_null() {
                pool.drain();
                return Err("file URL path is not valid UTF-8".to_owned());
            }
            paths.push(PathBuf::from(
                CStr::from_ptr(utf8).to_string_lossy().into_owned(),
            ));
        }
        pool.drain();
        Ok(paths)
    }
}

#[cfg(test)]
pub(crate) fn read_file_urls() -> Result<Vec<PathBuf>, String> {
    Ok(Vec::new())
}

#[cfg(not(test))]
pub(crate) fn write_selection(plain_text: &str, html: Option<&str>) -> Result<(), String> {
    let representations = selection_representations(plain_text, html);
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        let _: NSInteger = pasteboard.clearContents();
        for representation in representations {
            let value = NSString::alloc(nil)
                .init_str(representation.text)
                .autorelease();
            let pasteboard_type = match representation.mime {
                PLAIN_TEXT_MIME => NSPasteboardTypeString,
                HTML_MIME => NSPasteboardTypeHTML,
                _ => unreachable!("selection pasteboard MIME types are closed"),
            };
            let written = pasteboard.setString_forType(value, pasteboard_type) == YES;
            if !written {
                pool.drain();
                return Err(format!(
                    "macOS refused terminal selection representation {}",
                    representation.mime
                ));
            }
        }
        pool.drain();
    }
    Ok(())
}

#[cfg(not(test))]
#[derive(Debug, Default)]
pub(crate) struct MacosOsc52Clipboard;

#[cfg(not(test))]
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
                let pointer = value.UTF8String();
                if pointer.is_null() {
                    Err(Osc52ClipboardError::Unavailable)
                } else {
                    Ok(CStr::from_ptr(pointer).to_string_lossy().into_owned())
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
}
