//! Client accessibility uses the same exact styled rows, cursor, modes and PTY
//! hidden-input facts. No selection or native accessibility object is exported.
use spaceterm_core::content_queries::*;
impl crate::TerminalEmulator {
    pub fn accessibility_page(
        &mut self,
        request: &AccessibilityPageRequest,
    ) -> Result<AccessibilityPageResult, ContentReadFailure> {
        self.content_page(request)
    }
}
