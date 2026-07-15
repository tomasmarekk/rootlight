//! Higher-layer marker used by the forbidden-edge fixture.

#![forbid(unsafe_code)]

/// Returns the fixture marker value.
#[must_use]
pub const fn marker() -> bool {
    true
}
