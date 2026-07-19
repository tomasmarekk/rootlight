//! Checked identities and capabilities for the audited grammar families.
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
    /// Go grammar.
    Go,
    /// TypeScript grammar.
    TypeScript,
}

/// Stable parser-independent metadata for one registered grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarDescriptor {
    family: GrammarFamily,
    language: LanguageId,
    grammar_version: &'static str,
    grammar_source_sha256: &'static str,
    parser_sha256: &'static str,
    scanner_sha256: Option<&'static str>,
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

    /// Returns the exact crates.io source-package SHA-256 from the lockfile.
    #[must_use]
    pub const fn grammar_source_sha256(&self) -> &'static str {
        self.grammar_source_sha256
    }

    /// Returns the enforced generated `parser.c` SHA-256.
    #[must_use]
    pub const fn parser_sha256(&self) -> &'static str {
        self.parser_sha256
    }

    /// Returns the enforced generated scanner SHA-256 when one is linked.
    #[must_use]
    pub const fn scanner_sha256(&self) -> Option<&'static str> {
        self.scanner_sha256
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
        let mut descriptors = Vec::with_capacity(6);
        for family in [
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
            GrammarFamily::Go,
            GrammarFamily::TypeScript,
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
            let identity = identity_for(family);
            descriptors.push(GrammarDescriptor {
                family,
                language: LanguageId::new(identity.language_id)
                    .map_err(|_| RegistryError::InvalidBuiltInIdentity { family })?,
                grammar_version: identity.grammar_version,
                grammar_source_sha256: identity.source_package_sha256,
                parser_sha256: identity.parser_sha256,
                scanner_sha256: identity.scanner_sha256,
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
        GrammarFamily::Go => tree_sitter_go::LANGUAGE.into(),
        GrammarFamily::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    }
}

#[derive(Debug, Clone, Copy)]
struct GrammarIdentity {
    language_id: &'static str,
    grammar_version: &'static str,
    source_package_sha256: &'static str,
    parser_sha256: &'static str,
    scanner_sha256: Option<&'static str>,
}

const fn identity_for(family: GrammarFamily) -> GrammarIdentity {
    match family {
        GrammarFamily::Rust => GrammarIdentity {
            language_id: "rust",
            grammar_version: "0.24.2",
            source_package_sha256: "439e577dbe07423ec2582ac62c7531120dbfccfa6e5f92406f93dd271a120e45",
            parser_sha256: "9602518f9e57919910bf0e777e52f6bfc9325d4c182e998bdb4efd5682b76e4a",
            scanner_sha256: Some(
                "9609a2f92dbb7c32bc056fd8fb94e5478428f04496696aa08b048a9b66caf283",
            ),
        },
        GrammarFamily::Python => GrammarIdentity {
            language_id: "python",
            grammar_version: "0.25.0",
            source_package_sha256: "6bf85fd39652e740bf60f46f4cda9492c3a9ad75880575bf14960f775cb74a1c",
            parser_sha256: "a895f10b3cf7b2608f3283b43cd5cfed70971c7ee4a0136abbaaccbc4a7a25e0",
            scanner_sha256: Some(
                "6db82134ac2d4c90a1a1475487a625cface02662ebda9b7478cad9c7147e9afe",
            ),
        },
        GrammarFamily::JavaScript => GrammarIdentity {
            language_id: "javascript",
            grammar_version: "0.25.0",
            source_package_sha256: "68204f2abc0627a90bdf06e605f5c470aa26fdcb2081ea553a04bdad756693f5",
            parser_sha256: "67209ca7ef6e1a4f74e29e48b5928455f892fe1821a3960fbcd62f4e972f7384",
            scanner_sha256: Some(
                "b3d3f64284d97bf80749c026862427782cf7ecc0b7dc094e6698ab311c9a42c7",
            ),
        },
        GrammarFamily::Java => GrammarIdentity {
            language_id: "java",
            grammar_version: "0.23.5",
            source_package_sha256: "0aa6cbcdc8c679b214e616fd3300da67da0e492e066df01bcf5a5921a71e90d6",
            parser_sha256: "4add5150cf4531eb5dd97f3343dcf65cd11704c84711348b328582b83424a0e4",
            scanner_sha256: None,
        },
        GrammarFamily::Go => GrammarIdentity {
            language_id: "go",
            grammar_version: "0.25.0",
            source_package_sha256: "c8560a4d2f835cc0d4d2c2e03cbd0dde2f6114b43bc491164238d333e28b16ea",
            parser_sha256: "3dbf6ed1238b5dfcf2be4d2f2d4cb27a14d34f34d7784eccccbfd532fd4a6d85",
            scanner_sha256: None,
        },
        GrammarFamily::TypeScript => GrammarIdentity {
            language_id: "typescript",
            grammar_version: "0.23.2",
            source_package_sha256: "6c5f76ed8d947a75cc446d5fccd8b602ebf0cde64ccf2ffa434d873d7a575eff",
            parser_sha256: "74fe453edd70f4eae9af0a1050cbd7943d8971d59165b6aaebbaa0a0b716d1aa",
            scanner_sha256: Some(
                "9125013b42cb888379d9be909f1d73dfb75a37626c2cdbf4122718a2b431a6d3",
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    #[test]
    fn registry_contains_each_audited_family_once_with_checked_abi() {
        let registry = GrammarRegistry::audited().expect("audited grammars initialize");

        assert_eq!(registry.descriptors().len(), 6);
        for family in [
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
            GrammarFamily::Go,
            GrammarFamily::TypeScript,
        ] {
            let descriptor = registry.get(family).expect("family is registered");
            assert!(
                (TREE_SITTER_MIN_ABI..=TREE_SITTER_MAX_ABI).contains(&descriptor.abi_version())
            );
            assert_eq!(descriptor.encoding().as_str(), "utf-8");
            assert_eq!(descriptor.grammar_source_sha256().len(), 64);
            assert!(
                descriptor
                    .grammar_source_sha256()
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            );
        }
    }

    #[test]
    fn published_grammar_metadata_matches_enforced_locks() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let lock = fs::read_to_string(workspace.join("Cargo.lock"))
            .expect("workspace lockfile is readable");
        let grammar_lock = fs::read_to_string(workspace.join("adapters/grammars.lock"))
            .expect("grammar lockfile is readable");
        let (runtime_version, runtime_checksum) = locked_package(&lock, "tree-sitter");
        assert_eq!(runtime_version, crate::TREE_SITTER_RUNTIME_VERSION);
        let runtime_block = grammar_lock
            .split_once("[[grammars]]")
            .map(|(runtime, _grammars)| runtime)
            .expect("grammar lock contains audited grammars");
        assert_eq!(
            quoted_field(runtime_block, "crate_version"),
            crate::TREE_SITTER_RUNTIME_VERSION
        );
        assert_eq!(
            quoted_field(runtime_block, "crates_io_checksum"),
            runtime_checksum
        );

        let registry = GrammarRegistry::audited().expect("audited grammars initialize");
        for (family, language, package) in [
            (GrammarFamily::Rust, "rust", "tree-sitter-rust"),
            (GrammarFamily::Python, "python", "tree-sitter-python"),
            (
                GrammarFamily::JavaScript,
                "javascript",
                "tree-sitter-javascript",
            ),
            (GrammarFamily::Java, "java", "tree-sitter-java"),
            (GrammarFamily::Go, "go", "tree-sitter-go"),
            (
                GrammarFamily::TypeScript,
                "typescript",
                "tree-sitter-typescript",
            ),
        ] {
            let descriptor = registry.get(family).expect("family is registered");
            let (version, source_package_checksum) = locked_package(&lock, package);
            let grammar = locked_grammar(&grammar_lock, language);
            assert_eq!(descriptor.grammar_version(), version);
            assert_eq!(descriptor.grammar_source_sha256(), source_package_checksum);
            assert_eq!(quoted_field(grammar, "crate_version"), version);
            assert_eq!(
                quoted_field(grammar, "crates_io_checksum"),
                source_package_checksum
            );
            assert_eq!(
                descriptor.parser_sha256(),
                quoted_field(grammar, "parser_sha256")
            );
            let scanner = quoted_field(grammar, "scanner_sha256");
            assert_eq!(
                descriptor.scanner_sha256(),
                (scanner != "none").then_some(scanner)
            );
        }
    }

    fn locked_grammar<'a>(lock: &'a str, language: &str) -> &'a str {
        let marker = format!("language = \"{language}\"");
        let marker_start = lock.find(&marker).expect("locked grammar is present");
        let block_start = lock[..marker_start]
            .rfind("[[grammars]]")
            .expect("grammar block starts before its language");
        let block_tail = &lock[block_start..];
        let block_end = block_tail[1..]
            .find("[[grammars]]")
            .map_or(block_tail.len(), |offset| offset + 1);
        &block_tail[..block_end]
    }

    fn locked_package<'a>(lock: &'a str, package: &str) -> (&'a str, &'a str) {
        let marker = format!("name = \"{package}\"");
        let marker_start = lock.find(&marker).expect("locked package is present");
        let block_start = lock[..marker_start]
            .rfind("[[package]]")
            .expect("package block starts before its name");
        let block_tail = &lock[block_start..];
        let block_end = block_tail[1..]
            .find("[[package]]")
            .map_or(block_tail.len(), |offset| offset + 1);
        let block = &block_tail[..block_end];
        (
            quoted_field(block, "version"),
            quoted_field(block, "checksum"),
        )
    }

    fn quoted_field<'a>(block: &'a str, field: &str) -> &'a str {
        let prefix = format!("{field} = \"");
        let value = block
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .expect("locked package field is present");
        value
            .strip_suffix('"')
            .expect("locked package field is quoted")
    }
}
