//! Deliberately invalid architecture fixture.

#![forbid(unsafe_code)]

/// Calls the higher-layer fixture to create a forbidden edge.
pub fn invalid_edge() -> bool {
    fixture_higher::marker()
}
