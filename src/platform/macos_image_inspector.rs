//! Opens a Background Image with the system's decoders before SpaceTerm keeps a copy, so a copy
//! always presents and its decoded size is known. It runs on any thread.

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSBitmapImageRep;
use objc2_foundation::NSData;

use crate::background_image::ImageInspector;

pub(crate) struct MacosImageInspector;

impl ImageInspector for MacosImageInspector {
    fn pixel_size(&self, bytes: &[u8]) -> Option<(u64, u64)> {
        autoreleasepool(|_| {
            let image = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(bytes))?;
            let width = u64::try_from(image.pixelsWide()).ok()?;
            let height = u64::try_from(image.pixelsHigh()).ok()?;
            Some((width, height))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 PNG the system decodes.
    const PIXEL: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90\x77\x53\xde\x00\x00\x00\x0c\x49\x44\x41\x54\x78\x9c\x63\xf8\xdf\xc0\x00\x00\x04\x01\x01\x80\xc5\x2a\x18\x5d\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";

    #[test]
    fn a_decodable_image_reports_its_pixel_size_off_the_main_thread() {
        assert_eq!(MacosImageInspector.pixel_size(PIXEL), Some((1, 1)));
    }

    #[test]
    fn bytes_with_only_an_image_signature_do_not_open() {
        assert_eq!(
            MacosImageInspector.pixel_size(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            None
        );
        assert_eq!(
            MacosImageInspector.pixel_size(&PIXEL[..PIXEL.len() / 2]),
            None
        );
    }
}
