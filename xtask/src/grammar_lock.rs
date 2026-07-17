//! Verifies the reviewed Tree-sitter source, license, and Cargo dependency lock.
//!
//! The manifest is intentionally redundant with Cargo metadata: policy checks
//! fail if a dependency update changes either executable code or audit evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use cargo_metadata::{Dependency, Metadata};
use serde::Deserialize;
use sha2::Digest as _;

const GRAMMAR_LOCK_PATH: &str = "adapters/grammars.lock";
const CARGO_LOCK_PATH: &str = "Cargo.lock";
const ADAPTER_PACKAGE: &str = "rootlight-adapter-treesitter";
const GRAMMAR_LOCK_SHA256: &str =
    "925fcae685241c79da6ccba18d38a6f81e963cd2cad75434a65b28b50246ac8c";
const JAVA_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-java-0.23.5-LICENSE";
const JAVA_LICENSE_SHA256: &str =
    "52ed137b039cd9c46409bc22e89938af911c95b157feae2d040b51e6084369a7";
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

const EXPECTED_PACKAGES: [(&str, &str, &str); 5] = [
    (
        "tree-sitter",
        "0.26.11",
        "af1c71c1c4cc0920b20d6b0f6572e7682cd07a6a2faec71067a31fa394c586df",
    ),
    (
        "tree-sitter-java",
        "0.23.5",
        "0aa6cbcdc8c679b214e616fd3300da67da0e492e066df01bcf5a5921a71e90d6",
    ),
    (
        "tree-sitter-javascript",
        "0.25.0",
        "68204f2abc0627a90bdf06e605f5c470aa26fdcb2081ea553a04bdad756693f5",
    ),
    (
        "tree-sitter-python",
        "0.25.0",
        "6bf85fd39652e740bf60f46f4cda9492c3a9ad75880575bf14960f775cb74a1c",
    ),
    (
        "tree-sitter-rust",
        "0.24.2",
        "439e577dbe07423ec2582ac62c7531120dbfccfa6e5f92406f93dd271a120e45",
    ),
];

pub(crate) fn check(metadata: &Metadata, root: &Path) -> Result<(), GrammarLockError> {
    let path = root.join(GRAMMAR_LOCK_PATH);
    let bytes = fs::read(&path).map_err(|source| GrammarLockError::Read {
        path: path.clone(),
        source,
    })?;
    require_digest(GRAMMAR_LOCK_PATH, &bytes, GRAMMAR_LOCK_SHA256)?;
    let text = std::str::from_utf8(&bytes).map_err(|source| GrammarLockError::Utf8 {
        path: path.clone(),
        source,
    })?;
    let manifest: GrammarLock = toml::from_str(text).map_err(|source| GrammarLockError::Parse {
        path: path.clone(),
        source,
    })?;
    validate_manifest(&manifest)?;
    validate_direct_dependencies(metadata)?;
    validate_cargo_lock(root, &manifest)?;

    let java_license = root.join(JAVA_LICENSE_PATH);
    let license_bytes = fs::read(&java_license).map_err(|source| GrammarLockError::Read {
        path: java_license,
        source,
    })?;
    require_digest(JAVA_LICENSE_PATH, &license_bytes, JAVA_LICENSE_SHA256)?;
    Ok(())
}

fn validate_manifest(manifest: &GrammarLock) -> Result<(), GrammarLockError> {
    if manifest.schema_version != "1.0" {
        return Err(GrammarLockError::UnsupportedVersion(
            manifest.schema_version.clone(),
        ));
    }
    validate_runtime(&manifest.runtime)?;
    if manifest.grammars.len() != 4 {
        return Err(GrammarLockError::GrammarCount(manifest.grammars.len()));
    }
    let mut languages = BTreeSet::new();
    let mut packages = BTreeMap::new();
    for grammar in &manifest.grammars {
        validate_grammar(grammar)?;
        if !languages.insert(grammar.language.as_str()) {
            return Err(GrammarLockError::DuplicateLanguage(
                grammar.language.clone(),
            ));
        }
        packages.insert(
            grammar.crate_name.as_str(),
            (
                grammar.crate_version.as_str(),
                grammar.crates_io_checksum.as_str(),
            ),
        );
    }
    let expected_languages = BTreeSet::from(["java", "javascript", "python", "rust"]);
    if languages != expected_languages {
        return Err(GrammarLockError::LanguageSet);
    }
    for (name, version, checksum) in EXPECTED_PACKAGES.iter().skip(1) {
        if packages.get(name) != Some(&(*version, *checksum)) {
            return Err(GrammarLockError::PackageEvidence {
                package: (*name).to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_runtime(runtime: &RuntimeEvidence) -> Result<(), GrammarLockError> {
    let (expected_name, expected_version, expected_checksum) = EXPECTED_PACKAGES[0];
    if runtime.crate_name != expected_name
        || runtime.crate_version != expected_version
        || runtime.crates_io_checksum != expected_checksum
        || runtime.repository != "https://github.com/tree-sitter/tree-sitter"
        || runtime.tag != "v0.26.11"
        || runtime.commit != "64402de2857cc197ecc4ca3bc144ea91fda7e72e"
        || runtime.abi_minimum != 13
        || runtime.abi_current != 15
        || runtime.license != "MIT"
        || runtime.msrv != "1.77"
    {
        return Err(GrammarLockError::RuntimeEvidence);
    }
    validate_sha256("runtime crate checksum", &runtime.crates_io_checksum)?;
    validate_sha256("runtime license", &runtime.license_sha256)?;
    require_nonempty("runtime license_source", &runtime.license_source)?;
    require_nonempty("runtime offline_behavior", &runtime.offline_behavior)?;
    require_nonempty_collection("runtime capabilities", &runtime.capabilities)
}

fn validate_grammar(grammar: &GrammarEvidence) -> Result<(), GrammarLockError> {
    for (field, value) in [
        ("language", grammar.language.as_str()),
        ("crate_name", grammar.crate_name.as_str()),
        ("crate_version", grammar.crate_version.as_str()),
        ("repository", grammar.repository.as_str()),
        ("tag", grammar.tag.as_str()),
        ("commit", grammar.commit.as_str()),
        ("license", grammar.license.as_str()),
        ("license_source", grammar.license_source.as_str()),
        ("modifications", grammar.modifications.as_str()),
        ("test_corpus", grammar.test_corpus.as_str()),
        ("msrv", grammar.msrv.as_str()),
        ("offline_behavior", grammar.offline_behavior.as_str()),
    ] {
        require_nonempty(field, value)?;
    }
    validate_sha256("grammar crate checksum", &grammar.crates_io_checksum)?;
    validate_sha256("generated parser", &grammar.parser_sha256)?;
    if grammar.scanner_sha256 != "none" {
        validate_sha256("generated scanner", &grammar.scanner_sha256)?;
    }
    validate_sha256("grammar license", &grammar.license_sha256)?;
    if !(13..=15).contains(&grammar.abi) {
        return Err(GrammarLockError::GrammarAbi {
            language: grammar.language.clone(),
            abi: grammar.abi,
        });
    }
    require_nonempty_collection("grammar capabilities", &grammar.capabilities)?;
    if grammar.language == "rust" && grammar.audit_notes.len() != 2 {
        return Err(GrammarLockError::MissingAuditCaveat("rust"));
    }
    if grammar.language == "java"
        && (grammar.audit_notes.len() != 2 || grammar.license_source != JAVA_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("java"));
    }
    Ok(())
}

fn validate_direct_dependencies(metadata: &Metadata) -> Result<(), GrammarLockError> {
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == ADAPTER_PACKAGE)
        .ok_or(GrammarLockError::MissingAdapterPackage)?;
    validate_tree_sitter_dependencies(&package.dependencies)
}

fn validate_tree_sitter_dependencies(dependencies: &[Dependency]) -> Result<(), GrammarLockError> {
    let expected: BTreeMap<_, _> = EXPECTED_PACKAGES
        .iter()
        .map(|(name, version, _)| (*name, *version))
        .collect();
    let observed: Vec<_> = dependencies
        .iter()
        .filter(|dependency| {
            is_tree_sitter_package(&dependency.name)
                || dependency
                    .rename
                    .as_deref()
                    .is_some_and(is_tree_sitter_package)
        })
        .collect();
    let observed_names: BTreeSet<_> = observed
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();
    let expected_names: BTreeSet<_> = expected.keys().copied().collect();
    if observed.len() != expected.len() || observed_names != expected_names {
        return Err(GrammarLockError::DependencySet);
    }
    for (name, version) in expected {
        let dependency = observed
            .iter()
            .find(|dependency| dependency.name == name)
            .ok_or(GrammarLockError::DependencySet)?;
        validate_dependency_profile(dependency, version)?;
    }
    Ok(())
}

fn is_tree_sitter_package(name: &str) -> bool {
    name == "tree-sitter" || name.starts_with("tree-sitter-")
}

fn validate_dependency_profile(
    dependency: &Dependency,
    version: &str,
) -> Result<(), GrammarLockError> {
    if dependency.req.to_string() != format!("={version}")
        || dependency.kind != cargo_metadata::DependencyKind::Normal
        || dependency.optional
        || dependency.uses_default_features
        || dependency.rename.is_some()
        || dependency.target.is_some()
        || dependency.registry.is_some()
        || dependency
            .source
            .as_ref()
            .map(ToString::to_string)
            .as_deref()
            != Some(CRATES_IO_SOURCE)
    {
        return Err(GrammarLockError::DependencyProfile {
            package: dependency.name.clone(),
        });
    }
    let expected_features: &[&str] = if dependency.name == "tree-sitter" {
        &["std"]
    } else {
        &[]
    };
    if dependency
        .features
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        != expected_features
    {
        return Err(GrammarLockError::DependencyProfile {
            package: dependency.name.clone(),
        });
    }
    Ok(())
}

fn validate_cargo_lock(root: &Path, manifest: &GrammarLock) -> Result<(), GrammarLockError> {
    let path = root.join(CARGO_LOCK_PATH);
    let text = fs::read_to_string(&path).map_err(|source| GrammarLockError::Read {
        path: path.clone(),
        source,
    })?;
    let lock: CargoLock =
        toml::from_str(&text).map_err(|source| GrammarLockError::Parse { path, source })?;
    let observed: BTreeMap<_, _> = lock
        .package
        .iter()
        .filter_map(|package| {
            package
                .checksum
                .as_deref()
                .map(|checksum| ((package.name.as_str(), package.version.as_str()), checksum))
        })
        .collect();
    let mut expected = vec![(
        manifest.runtime.crate_name.as_str(),
        manifest.runtime.crate_version.as_str(),
        manifest.runtime.crates_io_checksum.as_str(),
    )];
    expected.extend(manifest.grammars.iter().map(|grammar| {
        (
            grammar.crate_name.as_str(),
            grammar.crate_version.as_str(),
            grammar.crates_io_checksum.as_str(),
        )
    }));
    for (name, version, checksum) in expected {
        if observed.get(&(name, version)) != Some(&checksum) {
            return Err(GrammarLockError::CargoLockChecksum {
                package: name.to_owned(),
                version: version.to_owned(),
            });
        }
    }
    Ok(())
}

fn require_digest(
    label: &'static str,
    bytes: &[u8],
    expected: &str,
) -> Result<(), GrammarLockError> {
    let observed = sha256_hex(bytes);
    if observed == expected {
        Ok(())
    } else {
        Err(GrammarLockError::DigestMismatch {
            label,
            expected: expected.to_owned(),
            observed,
        })
    }
}

fn validate_sha256(label: &'static str, value: &str) -> Result<(), GrammarLockError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(GrammarLockError::InvalidDigest { label })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = sha2::Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

fn require_nonempty(field: &'static str, value: &str) -> Result<(), GrammarLockError> {
    if value.trim().is_empty() {
        Err(GrammarLockError::EmptyField(field))
    } else {
        Ok(())
    }
}

fn require_nonempty_collection(
    field: &'static str,
    values: &[String],
) -> Result<(), GrammarLockError> {
    if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
        Err(GrammarLockError::EmptyField(field))
    } else {
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrammarLock {
    schema_version: String,
    runtime: RuntimeEvidence,
    grammars: Vec<GrammarEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeEvidence {
    crate_name: String,
    crate_version: String,
    crates_io_checksum: String,
    repository: String,
    tag: String,
    commit: String,
    abi_minimum: usize,
    abi_current: usize,
    license: String,
    license_source: String,
    license_sha256: String,
    msrv: String,
    offline_behavior: String,
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrammarEvidence {
    language: String,
    crate_name: String,
    crate_version: String,
    crates_io_checksum: String,
    repository: String,
    tag: String,
    commit: String,
    parser_sha256: String,
    scanner_sha256: String,
    abi: usize,
    license: String,
    license_source: String,
    license_sha256: String,
    modifications: String,
    test_corpus: String,
    msrv: String,
    offline_behavior: String,
    capabilities: Vec<String>,
    audit_notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoLock {
    package: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    checksum: Option<String>,
}

/// Failure to verify the audited grammar dependency lock.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GrammarLockError {
    #[error("failed to read grammar policy at {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("grammar policy at {} is not UTF-8", path.display())]
    Utf8 {
        path: PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("failed to parse grammar policy at {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported grammar lock schema {0}")]
    UnsupportedVersion(String),
    #[error("grammar lock digest for {label} differs: expected {expected}, observed {observed}")]
    DigestMismatch {
        label: &'static str,
        expected: String,
        observed: String,
    },
    #[error("{label} is not a lowercase SHA-256 digest")]
    InvalidDigest { label: &'static str },
    #[error("grammar lock field {0} must not be empty")]
    EmptyField(&'static str),
    #[error("grammar lock contains {0} grammars instead of four")]
    GrammarCount(usize),
    #[error("grammar lock repeats language {0}")]
    DuplicateLanguage(String),
    #[error("grammar lock does not contain the exact audited language set")]
    LanguageSet,
    #[error("runtime evidence differs from the audited Tree-sitter release")]
    RuntimeEvidence,
    #[error("package evidence differs for {package}")]
    PackageEvidence { package: String },
    #[error("grammar ABI {abi} is unsupported for {language}")]
    GrammarAbi { language: String, abi: usize },
    #[error("grammar lock omits the {0} audit caveat")]
    MissingAuditCaveat(&'static str),
    #[error("workspace metadata omits rootlight-adapter-treesitter")]
    MissingAdapterPackage,
    #[error("Tree-sitter direct dependency set differs from the grammar lock")]
    DependencySet,
    #[error("Tree-sitter dependency profile differs for {package}")]
    DependencyProfile { package: String },
    #[error("Cargo.lock checksum differs for {package}@{version}")]
    CargoLockChecksum { package: String, version: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn digest_validation_rejects_truncated_and_uppercase_values() {
        for invalid in [
            "abcd",
            "A25fcae685241c79da6ccba18d38a6f81e963cd2cad75434a65b28b50246ac8c",
        ] {
            assert!(matches!(
                validate_sha256("fixture", invalid),
                Err(GrammarLockError::InvalidDigest { label: "fixture" })
            ));
        }
    }

    #[test]
    fn dependency_set_rejects_an_unreviewed_tree_sitter_package() {
        let mut dependencies = audited_dependency_fixtures();
        dependencies.push(dependency_fixture("tree-sitter-unreviewed", "1.0.0", &[]));

        assert!(matches!(
            validate_tree_sitter_dependencies(&dependencies),
            Err(GrammarLockError::DependencySet)
        ));
    }

    #[test]
    fn dependency_profile_rejects_non_normal_optional_and_non_crates_io_forms() {
        let baseline = dependency_fixture("tree-sitter-rust", "0.24.2", &[]);
        let mut profiles = Vec::new();

        let mut development = baseline.clone();
        development.kind = cargo_metadata::DependencyKind::Development;
        profiles.push(development);

        let mut optional = baseline.clone();
        optional.optional = true;
        profiles.push(optional);

        let mut targeted = baseline.clone();
        targeted.target = Some(
            "cfg(windows)"
                .parse()
                .expect("test target expression is valid"),
        );
        profiles.push(targeted);

        let mut path = baseline.clone();
        path.source = None;
        profiles.push(path);

        let alternate_registry = dependency_fixture_with_source(
            "tree-sitter-rust",
            "0.24.2",
            &[],
            Some("registry+https://example.invalid/index"),
            Some("https://example.invalid/index"),
        );
        profiles.push(alternate_registry);

        for dependency in profiles {
            assert!(matches!(
                validate_dependency_profile(&dependency, "0.24.2"),
                Err(GrammarLockError::DependencyProfile { .. })
            ));
        }
    }

    fn audited_dependency_fixtures() -> Vec<Dependency> {
        EXPECTED_PACKAGES
            .iter()
            .map(|(name, version, _)| {
                let features = if *name == "tree-sitter" {
                    &["std"][..]
                } else {
                    &[][..]
                };
                dependency_fixture(name, version, features)
            })
            .collect()
    }

    fn dependency_fixture(name: &str, version: &str, features: &[&str]) -> Dependency {
        dependency_fixture_with_source(name, version, features, Some(CRATES_IO_SOURCE), None)
    }

    fn dependency_fixture_with_source(
        name: &str,
        version: &str,
        features: &[&str],
        source: Option<&str>,
        registry: Option<&str>,
    ) -> Dependency {
        serde_json::from_value(json!({
            "name": name,
            "source": source,
            "req": format!("={version}"),
            "kind": null,
            "rename": null,
            "optional": false,
            "uses_default_features": false,
            "features": features,
            "target": null,
            "registry": registry,
        }))
        .expect("test dependency metadata is valid")
    }
}
