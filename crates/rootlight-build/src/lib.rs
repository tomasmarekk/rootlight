//! Shared compile-time identity for shipped Rootlight components.

#![forbid(unsafe_code)]

/// Product release version embedded into every shipped component.
pub const PRODUCT_VERSION: &str = match option_env!("ROOTLIGHT_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Source revision supplied by reproducible candidate and release builds.
pub const SOURCE_REVISION: Option<&str> = option_env!("SOURCE_REVISION");

/// Product and component identity embedded into one binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildIdentity {
    /// Version shared by the package, CLI, MCP server, updater, and diagnostics.
    pub product_version: &'static str,
    /// Internal crate version useful when diagnosing mixed development builds.
    pub component_version: &'static str,
    /// Exact source revision when the build pipeline supplied one.
    pub source_revision: Option<&'static str>,
}

impl BuildIdentity {
    /// Creates the shared identity for a component's internal crate version.
    #[must_use]
    pub const fn current(component_version: &'static str) -> Self {
        Self {
            product_version: PRODUCT_VERSION,
            component_version,
            source_revision: SOURCE_REVISION,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_identity_uses_the_shared_product_version() {
        let identity = BuildIdentity::current("component-version");
        assert_eq!(identity.product_version, PRODUCT_VERSION);
        assert_eq!(identity.component_version, "component-version");
        assert!(!identity.product_version.is_empty());
    }
}
