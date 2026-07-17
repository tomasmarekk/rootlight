//! Checked identities and capabilities for the four audited grammar families.
//!
//! Runtime code resolves private Tree-sitter languages through this closed
//! registry; callers can inspect only stable, parser-independent descriptors.

use rootlight_adapter_sdk::{EncodingId, LanguageId};
use tree_sitter::Language;

const TREE_SITTER_MIN_ABI: usize = 13;
const TREE_SITTER_MAX_ABI: usize = 15;

/// A first-party grammar family audited for Rootlight's syntax fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum GrammarFamily {
    /// Rust grammar.
    Rust,
    /// Python grammar.
    Python,
    /// JavaScript grammar.
    JavaScript,
    /// Java grammar.
    Java,
}

/// Stable parser-independent metadata for one registered grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarDescriptor {
    family: GrammarFamily,
    language: LanguageId,
    grammar_version: &'static str,
    abi_version: usize,
    encoding: EncodingId,
}

impl GrammarDescriptor {
    /// Returns the grammar family.
    #[must_use]
    pub const fn family(&self) -> GrammarFamily {
        self.family
    }

    /// Returns the normalized language identity.
    #[must_use]
    pub const fn language(&self) -> &LanguageId {
        &self.language
    }

    /// Returns the exact grammar crate version.
    #[must_use]
    pub const fn grammar_version(&self) -> &'static str {
        self.grammar_version
    }

    /// Returns the generated parser ABI.
    #[must_use]
    pub const fn abi_version(&self) -> usize {
        self.abi_version
    }

    /// Returns the only admitted source encoding.
    #[must_use]
    pub const fn encoding(&self) -> &EncodingId {
        &self.encoding
    }
}

/// Closed registry of audited first-party grammars.
#[derive(Debug, Clone)]
pub struct GrammarRegistry {
    descriptors: Vec<GrammarDescriptor>,
}

impl GrammarRegistry {
    /// Builds the complete audited registry and verifies every native ABI.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if an SDK label is invalid or a linked grammar
    /// falls outside Tree-sitter's supported ABI interval.
    pub fn audited() -> Result<Self, RegistryError> {
        let mut descriptors = Vec::with_capacity(4);
        for family in [
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
        ] {
            let language = language_for(family);
            let abi_version = language.abi_version();
            if !(TREE_SITTER_MIN_ABI..=TREE_SITTER_MAX_ABI).contains(&abi_version) {
                return Err(RegistryError::UnsupportedAbi {
                    family,
                    observed: abi_version,
                    minimum: TREE_SITTER_MIN_ABI,
                    maximum: TREE_SITTER_MAX_ABI,
                });
            }
            let (language_id, grammar_version) = identity_for(family);
            descriptors.push(GrammarDescriptor {
                family,
                language: LanguageId::new(language_id)
                    .map_err(|_| RegistryError::InvalidBuiltInIdentity { family })?,
                grammar_version,
                abi_version,
                encoding: EncodingId::new("utf-8")
                    .map_err(|_| RegistryError::InvalidBuiltInIdentity { family })?,
            });
        }
        descriptors.sort_by_key(|descriptor| descriptor.family);
        Ok(Self { descriptors })
    }

    /// Returns descriptors in stable family order.
    #[must_use]
    pub fn descriptors(&self) -> &[GrammarDescriptor] {
        &self.descriptors
    }

    /// Finds one registered family.
    #[must_use]
    pub fn get(&self, family: GrammarFamily) -> Option<&GrammarDescriptor> {
        self.descriptors
            .binary_search_by_key(&family, |descriptor| descriptor.family)
            .ok()
            .and_then(|index| self.descriptors.get(index))
    }

    pub(crate) fn family_for_language(&self, language: &LanguageId) -> Option<GrammarFamily> {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.language == *language)
            .map(|descriptor| descriptor.family)
    }
}

/// Failure to initialize the audited grammar registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// A hard-coded SDK identity violated its own grammar.
    #[error("built-in grammar identity is invalid for {family:?}")]
    InvalidBuiltInIdentity {
        /// Affected grammar.
        family: GrammarFamily,
    },
    /// A generated grammar uses an unsupported Tree-sitter ABI.
    #[error(
        "{family:?} grammar ABI {observed} is outside supported interval {minimum}..={maximum}"
    )]
    UnsupportedAbi {
        /// Affected grammar.
        family: GrammarFamily,
        /// Linked grammar ABI.
        observed: usize,
        /// Minimum runtime ABI.
        minimum: usize,
        /// Maximum runtime ABI.
        maximum: usize,
    },
}

pub(crate) fn language_for(family: GrammarFamily) -> Language {
    match family {
        GrammarFamily::Rust => tree_sitter_rust::LANGUAGE.into(),
        GrammarFamily::Python => tree_sitter_python::LANGUAGE.into(),
        GrammarFamily::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        GrammarFamily::Java => tree_sitter_java::LANGUAGE.into(),
    }
}

const fn identity_for(family: GrammarFamily) -> (&'static str, &'static str) {
    match family {
        GrammarFamily::Rust => ("rust", "0.24.2"),
        GrammarFamily::Python => ("python", "0.25.0"),
        GrammarFamily::JavaScript => ("javascript", "0.25.0"),
        GrammarFamily::Java => ("java", "0.23.5"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_each_audited_family_once_with_checked_abi() {
        let registry = GrammarRegistry::audited().expect("audited grammars initialize");

        assert_eq!(registry.descriptors().len(), 4);
        for family in [
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
        ] {
            let descriptor = registry.get(family).expect("family is registered");
            assert!(
                (TREE_SITTER_MIN_ABI..=TREE_SITTER_MAX_ABI).contains(&descriptor.abi_version())
            );
            assert_eq!(descriptor.encoding().as_str(), "utf-8");
        }
    }
}
