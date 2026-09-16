//! Shared progress values and presentation state.

use std::{error::Error, fmt};

/// Error constructing a normalized determinate progress value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressValueError {
    /// NaN and positive or negative infinity cannot represent determinate progress.
    NotFinite,
}

impl fmt::Display for ProgressValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "determinate progress must be finite")
    }
}

impl Error for ProgressValueError {}

/// Finite normalized progress in the inclusive range `0.0..=1.0`.
///
/// ```
/// use spaceterm_ui::DeterminateProgress;
///
/// let progress = DeterminateProgress::new(1.25)?;
/// assert_eq!(progress.value(), 1.0);
/// assert!(progress.is_maximum());
/// # Ok::<(), spaceterm_ui::ProgressValueError>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeterminateProgress(f32);

impl DeterminateProgress {
    /// Normalizes a finite value by clamping it to the inclusive unit range.
    pub fn new(value: f64) -> Result<Self, ProgressValueError> {
        if !value.is_finite() {
            return Err(ProgressValueError::NotFinite);
        }
        Ok(Self(value.clamp(0.0, 1.0) as f32))
    }

    /// Returns the normalized finite value.
    pub const fn value(self) -> f32 {
        self.0
    }

    /// Returns whether the presentation reached the maximum value.
    ///
    /// Reaching one does not complete or hide the operation. The owner controls lifecycle.
    pub const fn is_maximum(self) -> bool {
        self.0 >= 1.0
    }
}

/// Progress with explicit determinate and indeterminate states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProgressState {
    /// Work has no knowable normalized completion value.
    Indeterminate,
    /// Work has a finite normalized completion value.
    Determinate(DeterminateProgress),
}
