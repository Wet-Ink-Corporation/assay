//! Line windows around a hunk.

/// Whether `line` lies within `window` lines of the hunk `first..=last`.
#[must_use]
pub fn near(line: u64, first: u64, last: u64, window: u64) -> bool {
    let from = first.saturating_sub(window);
    let to = last.saturating_add(window);
    line >= from && line < to
}
