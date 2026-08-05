#[cfg(not(test))]
use cocoa::appkit::{NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeString};
#[cfg(not(test))]
use cocoa::base::{YES, nil};
#[cfg(not(test))]
use cocoa::foundation::{NSAutoreleasePool, NSInteger, NSString};

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
