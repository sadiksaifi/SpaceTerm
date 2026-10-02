//! Private chrome faces and fontconfig fallback families for the Linux desktop.

use std::{
    ffi::{CStr, c_char},
    ptr,
};

use fontconfig_sys as fc;

const UI_FACES: &[&[u8]] = &[
    include_bytes!("../../assets/fonts/inter/Inter-Regular.otf"),
    include_bytes!("../../assets/fonts/inter/Inter-Medium.otf"),
    include_bytes!("../../assets/fonts/inter/Inter-SemiBold.otf"),
    include_bytes!("../../assets/fonts/inter/Inter-Bold.otf"),
];

pub(crate) fn capture() -> crate::host_fonts::HostFonts {
    crate::host_fonts::HostFonts {
        ui_family: "SpaceTerm UI".into(),
        system_monospace_family: family_match(c"monospace")
            .unwrap_or_else(|| crate::bundled_font::FAMILY.into()),
        terminal_families: &[crate::bundled_font::FAMILY],
        emoji_family: family_match(c"emoji").unwrap_or_else(|| crate::bundled_font::FAMILY.into()),
        bundled_ui_faces: UI_FACES,
    }
}

/// GPUI matches concrete family names, so resolve fontconfig aliases at composition.
fn family_match(family: &CStr) -> Option<String> {
    // SAFETY: All patterns are locally owned. Fontconfig copies the input string and the
    // returned family is copied before its pattern is destroyed. The null configuration
    // selects fontconfig's current configuration, initialized by fontconfig itself.
    unsafe {
        let request = Pattern(fc::FcPatternCreate());
        if request.0.is_null()
            || fc::FcPatternAddString(
                request.0,
                fc::constants::FC_FAMILY.as_ptr(),
                family.as_ptr().cast(),
            ) == 0
            || fc::FcConfigSubstitute(ptr::null_mut(), request.0, fc::FcMatchPattern) == 0
        {
            return None;
        }
        fc::FcDefaultSubstitute(request.0);
        let mut result = fc::FcResultNoMatch;
        let matched = Pattern(fc::FcFontMatch(ptr::null_mut(), request.0, &mut result));
        if matched.0.is_null() || result != fc::FcResultMatch {
            return None;
        }
        let mut name = ptr::null_mut();
        if fc::FcPatternGetString(matched.0, fc::constants::FC_FAMILY.as_ptr(), 0, &mut name)
            != fc::FcResultMatch
            || name.is_null()
        {
            return None;
        }
        let name = CStr::from_ptr(name.cast::<c_char>()).to_str().ok()?;
        (!name.is_empty()).then(|| name.to_owned())
    }
}

struct Pattern(*mut fc::FcPattern);

impl Drop for Pattern {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: This value owns the pattern and destroys it exactly once.
            unsafe { fc::FcPatternDestroy(self.0) };
        }
    }
}
