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
    /// JavaScript and JSX grammar with typed-syntax tolerance.
    JavaScript,
    /// Java grammar.
    Java,
    /// Go grammar.
    Go,
    /// TypeScript grammar.
    TypeScript,
    /// C grammar.
    C,
    /// C++ grammar.
    Cpp,
    /// C# grammar.
    CSharp,
    /// Kotlin grammar.
    Kotlin,
    /// PHP grammar with mixed HTML support.
    Php,
    /// Lua grammar.
    Lua,
    /// Ruby grammar.
    Ruby,
    /// Swift grammar with checked scanner state restoration.
    Swift,
    /// CSS grammar with CSS-defined Unicode and whitespace boundaries.
    Css,
    /// Bash grammar with checked scanner restoration and ordered heredoc inputs.
    Bash,
    /// JSON grammar with source-bound object members and array elements.
    Json,
    /// TOML grammar with source-bound keys, tables and array occurrences.
    Toml,
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
        let mut descriptors = Vec::with_capacity(18);
        for family in [
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
            GrammarFamily::Go,
            GrammarFamily::TypeScript,
            GrammarFamily::C,
            GrammarFamily::Cpp,
            GrammarFamily::CSharp,
            GrammarFamily::Kotlin,
            GrammarFamily::Php,
            GrammarFamily::Lua,
            GrammarFamily::Ruby,
            GrammarFamily::Swift,
            GrammarFamily::Css,
            GrammarFamily::Bash,
            GrammarFamily::Json,
            GrammarFamily::Toml,
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

pub(crate) fn native_family_for_source(family: GrammarFamily, path: &str) -> GrammarFamily {
    if family == GrammarFamily::TypeScript
        && path
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("tsx"))
    {
        // The JavaScript family already audits the TSX native parser. Sharing its
        // tree identity is safe; extraction still uses TypeScript's query contract.
        GrammarFamily::JavaScript
    } else {
        family
    }
}

pub(crate) fn language_for(family: GrammarFamily) -> Language {
    match family {
        GrammarFamily::Rust => tree_sitter_rust::LANGUAGE.into(),
        GrammarFamily::Python => tree_sitter_python::LANGUAGE.into(),
        // TSX accepts ordinary JavaScript and JSX while retaining declarations
        // around type annotations that otherwise trigger file-wide recovery.
        GrammarFamily::JavaScript => tree_sitter_typescript::LANGUAGE_TSX.into(),
        GrammarFamily::Java => tree_sitter_java::LANGUAGE.into(),
        GrammarFamily::Go => tree_sitter_go::LANGUAGE.into(),
        GrammarFamily::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        GrammarFamily::C => tree_sitter_c::LANGUAGE.into(),
        GrammarFamily::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        GrammarFamily::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        GrammarFamily::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
        GrammarFamily::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        GrammarFamily::Lua => tree_sitter_lua::LANGUAGE.into(),
        GrammarFamily::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        GrammarFamily::Swift => tree_sitter_swift::LANGUAGE.into(),
        GrammarFamily::Css => tree_sitter_css::LANGUAGE.into(),
        GrammarFamily::Bash => tree_sitter_bash::LANGUAGE.into(),
        GrammarFamily::Json => tree_sitter_json::LANGUAGE.into(),
        GrammarFamily::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
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
        GrammarFamily::Toml => GrammarIdentity {
            language_id: "toml",
            grammar_version: "0.7.0",
            source_package_sha256: "e9adc2c898ae49730e857d75be403da3f92bb81d8e37a2f918a08dd10de5ebb1",
            parser_sha256: "100fc1d97bc5dd6bc59e5d186010f505e4e26fcbe835ecfa3f244fe6fea3c002",
            scanner_sha256: Some(
                "b25ff3b5034f40046e9a041d1e9110aa46d081706bbd1b748b480701aa6f5bde",
            ),
        },
        GrammarFamily::Bash => GrammarIdentity {
            language_id: "bash",
            grammar_version: "0.25.1",
            source_package_sha256: "9e5ec769279cc91b561d3df0d8a5deb26b0ad40d183127f409494d6d8fc53062",
            parser_sha256: "87352349129188bcc98c5c18b805e3acc45bdcec936adaf1bd33cb034a5c767f",
            scanner_sha256: Some(
                "21f30fedabfa6a43bfddebf3ac59154b44c77b81fe42d23ff22bb2c0c3254ccc",
            ),
        },
        GrammarFamily::Json => GrammarIdentity {
            language_id: "json",
            grammar_version: "0.24.8",
            source_package_sha256: "4d727acca406c0020cffc6cf35516764f36c8e3dc4408e5ebe2cb35a947ec471",
            parser_sha256: "a3d0a4fbefb13d518e8e46b9b18b9b8bb16ece184faa3a543a4bbe18b466f7db",
            scanner_sha256: None,
        },
        GrammarFamily::Css => GrammarIdentity {
            language_id: "css",
            grammar_version: "0.25.0",
            source_package_sha256: "a5cbc5e18f29a2c6d6435891f42569525cf95435a3e01c2f1947abcde178686f",
            parser_sha256: "3563840da71829c8f883a262ead2cd7127e4a2ac09825b0a67484eebbcd40e58",
            scanner_sha256: Some(
                "6be764da4d1deb1d8561e1388cce38f2ca5d9312fc97d371b38afe96655a4ec1",
            ),
        },
        GrammarFamily::Swift => GrammarIdentity {
            language_id: "swift",
            grammar_version: "0.7.3",
            source_package_sha256: "fe36052155b9dd69ca82b3b8f1b4ccfb2d867125ac1a4db1dd7331829242668c",
            parser_sha256: "d3edff6effe31b9a507f496577407987343b101b23eb7bee7a9b050e8ab5d27a",
            scanner_sha256: Some(
                "de87dbc697465445bb45e86dc002859eaae6f64bf102acd3beeab4e044c906e7",
            ),
        },
        GrammarFamily::Ruby => GrammarIdentity {
            language_id: "ruby",
            grammar_version: "0.23.1",
            source_package_sha256: "be0484ea4ef6bb9c575b4fdabde7e31340a8d2dbc7d52b321ac83da703249f95",
            parser_sha256: "4ce468358b6f4e25a35c8cf6bc0eaf60665bc22d602f8c939323c2347255cd15",
            scanner_sha256: Some(
                "e7a6196d6e78bf4c6728502e924c867dee5d851c6253e43fcdb8ba169009bc58",
            ),
        },
        GrammarFamily::Lua => GrammarIdentity {
            language_id: "lua",
            grammar_version: "0.5.0",
            source_package_sha256: "8daaf5f4235188a58603c39760d5fa5d4b920d36a299c934adddae757f32a10c",
            parser_sha256: "933206d96a78f7785c13b2600182f1527dcd755c200b1271bb5bc4d8da4b17b3",
            scanner_sha256: Some(
                "35bbd630b5a7421d46d2e91185eeea09bf78565d44cb676b63ca20d0f1b54bbd",
            ),
        },
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
            grammar_version: "0.23.2",
            source_package_sha256: "6c5f76ed8d947a75cc446d5fccd8b602ebf0cde64ccf2ffa434d873d7a575eff",
            parser_sha256: "1902cb53fa7ff5179df89b2eea863165e84c8cc866226419dc26921d8c055885",
            scanner_sha256: Some(
                "d563cd30b2f39718c9ae4292795c5ce03a2ad01954ba3a86ef84c2781a736673",
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
        GrammarFamily::C => GrammarIdentity {
            language_id: "c",
            grammar_version: "0.24.2",
            source_package_sha256: "a9b2eb57a55fed6b00812912e730b7a275cf4fe98bfd6a5d76263d4438371728",
            parser_sha256: "f2883ff9b21f4a5bd5553c1b10366c418947d17a7bc6bf256124a7542f079dd2",
            scanner_sha256: None,
        },
        GrammarFamily::Cpp => GrammarIdentity {
            language_id: "cpp",
            grammar_version: "0.23.4",
            source_package_sha256: "df2196ea9d47b4ab4a31b9297eaa5a5d19a0b121dceb9f118f6790ad0ab94743",
            parser_sha256: "2a35a43b4af6c9f7b69624ac00c2c50808912591450dc79c05dea03ac1bae814",
            scanner_sha256: Some(
                "cf60387d290271f4d2fb558d0569b2b9ef879cb72206a9df5cc73b0b1a4b20ff",
            ),
        },
        GrammarFamily::CSharp => GrammarIdentity {
            language_id: "csharp",
            grammar_version: "0.23.5",
            source_package_sha256: "c1aac67f1ad71de1d6d39708d34811081c26dfa495658de6c14c34200849357c",
            parser_sha256: "0a2651e49de7c7237c535c41a132a7ec0424da79d12f197f1df3edd7d6ea4427",
            scanner_sha256: Some(
                "00920daa0fdf3050d441475e8effe996b60ee8acc1bdbcbbc4c83d56a6987705",
            ),
        },
        GrammarFamily::Kotlin => GrammarIdentity {
            language_id: "kotlin",
            grammar_version: "1.1.0",
            source_package_sha256: "e800ebbda938acfbf224f4d2c34947a31994b1295ee6e819b65226c7b51b4450",
            parser_sha256: "9ff65161845b9e9c9d62c12e9a4e4b8d8628bdc31c681ec7e6b4bd3bd6444cb3",
            scanner_sha256: Some(
                "164c2bb928d23765ed38c3a322deaef9b52d6e147cec7cdf45add90d4fa6101c",
            ),
        },
        GrammarFamily::Php => GrammarIdentity {
            language_id: "php",
            grammar_version: "0.24.2",
            source_package_sha256: "0d8c17c3ab69052c5eeaa7ff5cd972dd1bc25d1b97ee779fec391ad3b5df5592",
            parser_sha256: "59ad8e5e4fde3fe60687a488ab8420612840cc966b83739af1b3a4317ed27ec6",
            scanner_sha256: Some(
                "58c92cafe4ebda509c3ad3864fa6fc0e9877bbac26e17a03d23ea2101c291ad5",
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    #[test]
    fn tsx_native_selection_preserves_declared_language_and_plain_typescript_syntax() {
        for (family, path, expected) in [
            (GrammarFamily::TypeScript, "tsx", GrammarFamily::TypeScript),
            (
                GrammarFamily::TypeScript,
                "view.tsx",
                GrammarFamily::JavaScript,
            ),
            (
                GrammarFamily::TypeScript,
                "view.TSX",
                GrammarFamily::JavaScript,
            ),
            (
                GrammarFamily::TypeScript,
                "view.tsx/file.ts",
                GrammarFamily::TypeScript,
            ),
            (
                GrammarFamily::TypeScript,
                "convert.mts",
                GrammarFamily::TypeScript,
            ),
            (
                GrammarFamily::TypeScript,
                "convert.cts",
                GrammarFamily::TypeScript,
            ),
            (GrammarFamily::Rust, "view.tsx", GrammarFamily::Rust),
        ] {
            assert_eq!(native_family_for_source(family, path), expected, "{path}");
        }
    }

    #[test]
    fn registry_contains_each_audited_family_once_with_checked_abi() {
        let registry = GrammarRegistry::audited().expect("audited grammars initialize");

        assert_eq!(registry.descriptors().len(), 18);
        for family in [
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
            GrammarFamily::Go,
            GrammarFamily::TypeScript,
            GrammarFamily::C,
            GrammarFamily::Cpp,
            GrammarFamily::CSharp,
            GrammarFamily::Kotlin,
            GrammarFamily::Php,
            GrammarFamily::Lua,
            GrammarFamily::Ruby,
            GrammarFamily::Swift,
            GrammarFamily::Css,
            GrammarFamily::Bash,
            GrammarFamily::Json,
            GrammarFamily::Toml,
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
        let runtime_checksum = runtime_checksum.expect("runtime stays registry-backed");
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
                "tree-sitter-typescript",
            ),
            (GrammarFamily::Java, "java", "tree-sitter-java"),
            (GrammarFamily::Go, "go", "tree-sitter-go"),
            (
                GrammarFamily::TypeScript,
                "typescript",
                "tree-sitter-typescript",
            ),
            (GrammarFamily::C, "c", "tree-sitter-c"),
            (GrammarFamily::Cpp, "cpp", "tree-sitter-cpp"),
            (GrammarFamily::CSharp, "csharp", "tree-sitter-c-sharp"),
            (GrammarFamily::Kotlin, "kotlin", "tree-sitter-kotlin-ng"),
            (GrammarFamily::Php, "php", "tree-sitter-php"),
            (GrammarFamily::Lua, "lua", "tree-sitter-lua"),
            (GrammarFamily::Ruby, "ruby", "tree-sitter-ruby"),
            (GrammarFamily::Swift, "swift", "tree-sitter-swift"),
            (GrammarFamily::Css, "css", "tree-sitter-css"),
            (GrammarFamily::Bash, "bash", "tree-sitter-bash"),
            (GrammarFamily::Json, "json", "tree-sitter-json"),
            (GrammarFamily::Toml, "toml", "tree-sitter-toml-ng"),
        ] {
            let descriptor = registry.get(family).expect("family is registered");
            let (version, source_package_checksum) = locked_package(&lock, package);
            let grammar = locked_grammar(&grammar_lock, language);
            assert_eq!(descriptor.grammar_version(), version);
            if let Some(checksum) = source_package_checksum {
                assert_eq!(descriptor.grammar_source_sha256(), checksum);
            } else {
                // Local overrides retain the upstream archive identity, while
                // xtask verifies every vendored byte against the grammar lock.
                assert!(quoted_field(grammar, "manifest_path").starts_with("third_party/"));
            }
            assert_eq!(quoted_field(grammar, "crate_version"), version);
            assert_eq!(
                quoted_field(grammar, "crates_io_checksum"),
                descriptor.grammar_source_sha256()
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

    fn locked_package<'a>(lock: &'a str, package: &str) -> (&'a str, Option<&'a str>) {
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
            block.lines().find_map(|line| {
                line.strip_prefix("checksum = \"")
                    .and_then(|value| value.strip_suffix('"'))
            }),
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
