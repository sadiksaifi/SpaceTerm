//! Native macOS microbenchmark for SpaceTerm's startup font classification.
//! This mirrors appearance_runtime::capture_fonts without opening the application.

use gpui::{App, Application, font, px};
use std::hint::black_box;
use std::time::Instant;

const DEFAULT_TERMINAL_FAMILIES: [&str; 4] = [
    "JetBrainsMono Nerd Font",
    "JetBrainsMono Nerd Font Mono",
    "JetBrains Mono",
    "Menlo",
];

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "full".to_owned());
    assert!(matches!(
        mode.as_str(),
        "listing"
            | "full"
            | "selected"
            | "families-descriptors"
            | "families-direct"
            | "families-attributes"
            | "families-compare"
    ));

    Application::new().run(move |cx| measure(&mode, cx));
}

fn measure(mode: &str, cx: &mut App) {
    if mode.starts_with("families-") {
        #[cfg(target_os = "macos")]
        native_families::measure(mode, cx);
        #[cfg(not(target_os = "macos"))]
        panic!("native family comparison requires macOS");
        return;
    }
    let start = Instant::now();
    let text = cx.text_system();
    let names = text.all_font_names();
    let name_count = names.len();
    let mut classified = 0;
    if mode != "listing" {
        for family in names {
            if mode == "selected" && !DEFAULT_TERMINAL_FAMILIES.contains(&family.as_str()) {
                continue;
            }
            let id = text.resolve_font(&font(family.clone()));
            let widths = ['i', 'M', '0', ' '].map(|ch| text.advance(id, px(18.0), ch));
            let monospace = widths.iter().all(|width| width.is_ok())
                && widths.windows(2).all(|pair| {
                    (f32::from(pair[0].as_ref().unwrap().width)
                        - f32::from(pair[1].as_ref().unwrap().width))
                    .abs()
                        < 0.01
                });
            black_box((format!("{id:?}"), family, monospace));
            classified += 1;
        }
    } else {
        black_box(names);
    }
    println!(
        "native_font_classification mode={mode} names={name_count} classified={classified} elapsed_us={}",
        start.elapsed().as_micros()
    );
    cx.quit();
}

#[cfg(target_os = "macos")]
mod native_families {
    use gpui::App;
    use std::ffi::{CStr, c_char, c_void};
    use std::hint::black_box;
    use std::time::Instant;

    // This fixture never calls add_fonts. A production change must keep GPUI's memory_source
    // extension unchanged. These are the additions in TextSystem::all_font_names/new, not a
    // replacement installed-font catalog. Compare counts only, keeping font names out of output.
    const GPUI_ADDITIONAL_FAMILIES: [&str; 11] = [
        ".ZedMono",
        ".ZedSans",
        "Helvetica",
        "Segoe UI",
        "Ubuntu",
        "Adwaita Sans",
        "Cantarell",
        "Noto Sans",
        "DejaVu Sans",
        "Arial",
        ".SystemUIFont",
    ];

    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontManagerCopyAvailableFontFamilyNames() -> *const c_void;
        fn CTFontCollectionCreateFromAvailableFonts(options: *const c_void) -> *const c_void;
        fn CTFontCollectionCreateMatchingFontDescriptors(
            collection: *const c_void,
        ) -> *const c_void;
        fn CTFontDescriptorCopyAttribute(
            descriptor: *const c_void,
            attribute: *const c_void,
        ) -> *const c_void;
        fn CTFontCollectionCopyFontAttribute(
            collection: *const c_void,
            attribute: *const c_void,
            options: u32,
        ) -> *const c_void;
        static kCTFontFamilyNameAttribute: *const c_void;
        static kCTFontCollectionRemoveDuplicatesOption: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFStringGetLength(string: *const c_void) -> isize;
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(value: *const c_void);
        fn CFGetTypeID(value: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
        static kCFNull: *const c_void;
        fn CFNumberCreate(
            allocator: *const c_void,
            number_type: isize,
            value: *const c_void,
        ) -> *const c_void;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }

    struct RetainedCf(*const c_void);

    impl Drop for RetainedCf {
        fn drop(&mut self) {
            // SAFETY: a Create/Copy function returns one owned reference, released only here.
            unsafe { CFRelease(self.0) };
        }
    }

    fn direct_names() -> Vec<String> {
        // SAFETY: this CoreText function has no arguments and returns a retained CFArray of
        // CFStrings, or null. Each borrowed element stays alive until RetainedCf is dropped.
        let array = unsafe { CTFontManagerCopyAvailableFontFamilyNames() };
        assert!(!array.is_null(), "native font enumeration failed");
        names_from_array(RetainedCf(array))
    }

    fn attribute_names() -> Vec<String> {
        let collection = available_collection();
        // SAFETY: the collection and static attribute are valid. Option 1 is CopyUnique, which
        // removes duplicate values. This returns a retained array with kCFNull for absent values.
        let array = unsafe {
            CTFontCollectionCopyFontAttribute(collection.0, kCTFontFamilyNameAttribute, 1)
        };
        assert!(!array.is_null(), "native font attribute enumeration failed");
        names_from_array(RetainedCf(array))
    }

    fn descriptor_names() -> Vec<String> {
        // Keep the old per-descriptor algorithm independent of the production GPUI method.
        let collection = available_collection();
        let mut names = Vec::new();
        // SAFETY: this unusually named Create function follows the Get rule. Its returned array
        // is borrowed from the live collection. Copied attributes own their individual references.
        unsafe {
            let descriptors = CTFontCollectionCreateMatchingFontDescriptors(collection.0);
            assert!(
                !descriptors.is_null(),
                "native font descriptors unavailable"
            );
            for index in 0..CFArrayGetCount(descriptors) {
                let descriptor = CFArrayGetValueAtIndex(descriptors, index);
                let family = CTFontDescriptorCopyAttribute(descriptor, kCTFontFamilyNameAttribute);
                if !family.is_null() {
                    let family = RetainedCf(family);
                    names.push(string_from_cf(family.0));
                }
            }
        }
        finish_names(names)
    }

    fn available_collection() -> RetainedCf {
        // Match core-text::create_for_all_families exactly, including its duplicate-filter option.
        // SAFETY: all inputs use CoreFoundation's documented types and lifetimes. Callback
        // addresses refer to the native callback structures; Rust never dereferences those
        // structures. The dictionary retains its key/value, and each Create result is owned.
        unsafe {
            let one = 1_i64;
            let number = CFNumberCreate(std::ptr::null(), 4, (&one as *const i64).cast());
            assert!(!number.is_null(), "native font options number failed");
            let number = RetainedCf(number);
            let keys = [kCTFontCollectionRemoveDuplicatesOption];
            let values = [number.0];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks),
                std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks),
            );
            assert!(!options.is_null(), "native font collection options failed");
            let options = RetainedCf(options);
            let collection = CTFontCollectionCreateFromAvailableFonts(options.0);
            assert!(!collection.is_null(), "native font collection failed");
            RetainedCf(collection)
        }
    }

    fn names_from_array(array: RetainedCf) -> Vec<String> {
        let mut names = Vec::new();
        // SAFETY: array is the non-null CFArray returned by CoreText; indices stay within count.
        unsafe {
            for index in 0..CFArrayGetCount(array.0) {
                let family = CFArrayGetValueAtIndex(array.0, index);
                if family == kCFNull {
                    continue;
                }
                names.push(string_from_cf(family));
            }
        }
        finish_names(names)
    }

    /// The caller must retain the non-null CoreFoundation object for this call.
    unsafe fn string_from_cf(family: *const c_void) -> String {
        // SAFETY: the caller retains a valid object. Check its type before string operations.
        unsafe {
            assert_eq!(
                CFGetTypeID(family),
                CFStringGetTypeID(),
                "native font attribute is not a string"
            );
            // Four UTF-8 bytes per UTF-16 code unit plus NUL is a conservative bound.
            let capacity = usize::try_from(CFStringGetLength(family))
                .expect("invalid native font name length")
                .checked_mul(4)
                .and_then(|length| length.checked_add(1))
                .expect("native font name exceeds capacity");
            let mut buffer = vec![0_u8; capacity];
            let converted = CFStringGetCString(
                family,
                buffer.as_mut_ptr().cast(),
                isize::try_from(capacity).expect("native font name exceeds capacity"),
                0x0800_0100, // kCFStringEncodingUTF8
            );
            assert_ne!(converted, 0, "native font name conversion failed");
            CStr::from_ptr(buffer.as_ptr().cast())
                .to_str()
                .expect("native font name is not UTF-8")
                .to_owned()
        }
    }

    fn finish_names(mut names: Vec<String>) -> Vec<String> {
        names.extend(GPUI_ADDITIONAL_FAMILIES.map(str::to_owned));
        names.sort();
        names.dedup();
        names
    }

    pub(super) fn measure(mode: &str, cx: &mut App) {
        if mode == "families-compare" {
            let descriptors = descriptor_names();
            let direct = direct_names();
            let attributes = attribute_names();
            let production = cx.text_system().all_font_names();
            let missing = descriptors
                .iter()
                .filter(|name| direct.binary_search(name).is_err())
                .count();
            let added = direct
                .iter()
                .filter(|name| descriptors.binary_search(name).is_err())
                .count();
            println!(
                "native_font_family_parity descriptor_names={} direct_names={} missing={} added={} equal={}",
                descriptors.len(),
                direct.len(),
                missing,
                added,
                descriptors == direct
            );
            let missing_attributes = descriptors
                .iter()
                .filter(|name| attributes.binary_search(name).is_err())
                .count();
            let added_attributes = attributes
                .iter()
                .filter(|name| descriptors.binary_search(name).is_err())
                .count();
            println!(
                "native_font_attribute_parity descriptor_names={} attribute_names={} missing={} added={} equal={}",
                descriptors.len(),
                attributes.len(),
                missing_attributes,
                added_attributes,
                descriptors == attributes
            );
            assert!(
                descriptors == attributes,
                "native font attribute sets differ"
            );
            println!(
                "native_font_production_parity production_names={} equal={}",
                production.len(),
                production == descriptors
            );
            assert!(
                descriptors == production,
                "production font family sets differ"
            );
        } else {
            let start = Instant::now();
            let names = if mode == "families-direct" {
                direct_names()
            } else if mode == "families-attributes" {
                attribute_names()
            } else {
                descriptor_names()
            };
            let elapsed = start.elapsed();
            println!(
                "native_font_family_enumeration mode={mode} names={} elapsed_us={}",
                names.len(),
                elapsed.as_micros()
            );
            black_box(names);
        }
        cx.quit();
    }
}
