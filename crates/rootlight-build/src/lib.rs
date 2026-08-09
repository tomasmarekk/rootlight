//! Shared compile-time identity for shipped Rootlight components.

#![forbid(unsafe_code)]

/// Product release version embedded into every shipped component.
pub const PRODUCT_VERSION: &str = match option_env!("ROOTLIGHT_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Source revision supplied by reproducible candidate and release builds.
pub const SOURCE_REVISION: Option<&str> = validated_source_revision(option_env!("SOURCE_REVISION"));

const fn validated_source_revision(source_revision: Option<&'static str>) -> Option<&'static str> {
    match source_revision {
        Some(source_revision) => {
            assert!(
                is_canonical_source_revision(source_revision),
                "SOURCE_REVISION must be a canonical 40- or 64-character lowercase hex digest"
            );
            Some(source_revision)
        }
        None => None,
    }
}

const fn is_canonical_source_revision(source_revision: &str) -> bool {
    if !matches!(source_revision.len(), 40 | 64) {
        return false;
    }
    let bytes = source_revision.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !matches!(bytes[index], b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
        index += 1;
    }
    true
}

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

    #[test]
    fn source_revision_accepts_canonical_sha_digests() {
        for revision in [
            "0123456789abcdef0123456789abcdef01234567",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(is_canonical_source_revision(revision));
        }
    }

    #[test]
    fn source_revision_rejects_noncanonical_values() {
        for revision in [
            "",
            "0123456789abcdef0123456789abcdef0123456",
            "0123456789abcdef0123456789abcdef012345678",
            "0123456789abcdef0123456789abcdef0123456g",
            "0123456789abcdef0123456789abcdef0123456A",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0",
        ] {
            assert!(!is_canonical_source_revision(revision));
        }
    }
}
