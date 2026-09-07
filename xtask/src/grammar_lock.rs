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
    "0e780485ea1b1704110b6d1acf29e055b883bb6c87ad426c5fca372a430d9556";
const JAVA_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-java-0.23.5-LICENSE";
const JAVA_LICENSE_SHA256: &str =
    "52ed137b039cd9c46409bc22e89938af911c95b157feae2d040b51e6084369a7";
const TYPESCRIPT_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-typescript-0.23.2-LICENSE";
const TYPESCRIPT_LICENSE_SHA256: &str =
    "49bf33cf78ef5897e4e161ce1517df7de1ae5042a65b6bcfd44401e0fc606559";
const CPP_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-cpp-0.23.4-LICENSE";
const CPP_LICENSE_SHA256: &str = "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1";
const KOTLIN_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-kotlin-ng-1.1.0-LICENSE";
const KOTLIN_LICENSE_SHA256: &str =
    "0eea8dc45e89deeb03c7799bbbc7b4688f365fb274562f4540ecfebdea82e727";
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const LUA_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-lua-0.5.0-LICENSE";
const LUA_LICENSE_SHA256: &str = "9a32b02e4c917b1ce6b5e79d8ea81e25cefd7f27d89c7235f2afb262c06cf32e";

const RUBY_LICENSE_PATH: &str = "adapters/licenses/tree-sitter-ruby-0.23.1-LICENSE";
const RUBY_LICENSE_SHA256: &str =
    "ee006f02a3d856df282e409be2a86e24a65bb573a98b9c28343771141351bb6b";

const EXPECTED_PACKAGES: [(&str, &str, &str); 20] = [
    (
        "tree-sitter",
        "0.26.11",
        "af1c71c1c4cc0920b20d6b0f6572e7682cd07a6a2faec71067a31fa394c586df",
    ),
    (
        "tree-sitter-bash",
        "0.25.1",
        "9e5ec769279cc91b561d3df0d8a5deb26b0ad40d183127f409494d6d8fc53062",
    ),
    (
        "tree-sitter-c",
        "0.24.2",
        "a9b2eb57a55fed6b00812912e730b7a275cf4fe98bfd6a5d76263d4438371728",
    ),
    (
        "tree-sitter-c-sharp",
        "0.23.5",
        "c1aac67f1ad71de1d6d39708d34811081c26dfa495658de6c14c34200849357c",
    ),
    (
        "tree-sitter-cpp",
        "0.23.4",
        "df2196ea9d47b4ab4a31b9297eaa5a5d19a0b121dceb9f118f6790ad0ab94743",
    ),
    (
        "tree-sitter-go",
        "0.25.0",
        "c8560a4d2f835cc0d4d2c2e03cbd0dde2f6114b43bc491164238d333e28b16ea",
    ),
    (
        "tree-sitter-java",
        "0.23.5",
        "0aa6cbcdc8c679b214e616fd3300da67da0e492e066df01bcf5a5921a71e90d6",
    ),
    (
        "tree-sitter-json",
        "0.24.8",
        "4d727acca406c0020cffc6cf35516764f36c8e3dc4408e5ebe2cb35a947ec471",
    ),
    (
        "tree-sitter-kotlin-ng",
        "1.1.0",
        "e800ebbda938acfbf224f4d2c34947a31994b1295ee6e819b65226c7b51b4450",
    ),
    (
        "tree-sitter-php",
        "0.24.2",
        "0d8c17c3ab69052c5eeaa7ff5cd972dd1bc25d1b97ee779fec391ad3b5df5592",
    ),
    (
        "tree-sitter-lua",
        "0.5.0",
        "8daaf5f4235188a58603c39760d5fa5d4b920d36a299c934adddae757f32a10c",
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
    (
        "tree-sitter-ruby",
        "0.23.1",
        "be0484ea4ef6bb9c575b4fdabde7e31340a8d2dbc7d52b321ac83da703249f95",
    ),
    (
        "tree-sitter-typescript",
        "0.23.2",
        "6c5f76ed8d947a75cc446d5fccd8b602ebf0cde64ccf2ffa434d873d7a575eff",
    ),
    (
        "tree-sitter-toml-ng",
        "0.7.0",
        "e9adc2c898ae49730e857d75be403da3f92bb81d8e37a2f918a08dd10de5ebb1",
    ),
    (
        "tree-sitter-swift",
        "0.7.3",
        "fe36052155b9dd69ca82b3b8f1b4ccfb2d867125ac1a4db1dd7331829242668c",
    ),
    (
        "tree-sitter-css",
        "0.25.0",
        "a5cbc5e18f29a2c6d6435891f42569525cf95435a3e01c2f1947abcde178686f",
    ),
    (
        "tree-sitter-yaml",
        "0.7.2",
        "53c223db85f05e34794f065454843b0668ebc15d240ada63e2b5939f43ce7c97",
    ),
    (
        "tree-sitter-html",
        "0.23.2",
        "261b708e5d92061ede329babaaa427b819329a9d427a1d710abb0f67bbef63ee",
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
    validate_vendored_sources(metadata, root, &manifest)?;

    validate_local_license(root, JAVA_LICENSE_PATH, JAVA_LICENSE_SHA256)?;
    validate_local_license(root, TYPESCRIPT_LICENSE_PATH, TYPESCRIPT_LICENSE_SHA256)?;
    validate_local_license(root, CPP_LICENSE_PATH, CPP_LICENSE_SHA256)?;
    validate_local_license(root, KOTLIN_LICENSE_PATH, KOTLIN_LICENSE_SHA256)?;
    validate_local_license(root, LUA_LICENSE_PATH, LUA_LICENSE_SHA256)?;
    validate_local_license(root, RUBY_LICENSE_PATH, RUBY_LICENSE_SHA256)?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-html-0.23.2-LICENSE",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-yaml-0.7.2-LICENSE",
        "2d181298822a124f6d106118dcc69c8d4d26ac668559be29682c452e712b0947",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-toml-ng-0.7.0-LICENSE",
        "b727eee929bee836e8102cecf749a795b199f291b58c885bcd81cae727ac5a46",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-json-0.24.8-LICENSE",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-bash-0.25.1-LICENSE",
        "49bf33cf78ef5897e4e161ce1517df7de1ae5042a65b6bcfd44401e0fc606559",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-css-0.25.0-LICENSE",
        "c5cfb43042b6b72045f4ba997834d0a7786d2793d91680868b5815b39f14fc78",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-swift-0.7.3-LICENSE",
        "3533cec129bb4bba015c0d61d86dd7c3b7e82110e4d2ff7837a01eff5bad5ccc",
    )?;
    Ok(())
}

fn validate_local_license(
    root: &Path,
    relative_path: &'static str,
    expected_sha256: &str,
) -> Result<(), GrammarLockError> {
    let path = root.join(relative_path);
    let bytes = fs::read(&path).map_err(|source| GrammarLockError::Read { path, source })?;
    require_digest(relative_path, &bytes, expected_sha256)
}

fn validate_manifest(manifest: &GrammarLock) -> Result<(), GrammarLockError> {
    if manifest.schema_version != "1.0" {
        return Err(GrammarLockError::UnsupportedVersion(
            manifest.schema_version.clone(),
        ));
    }
    validate_runtime(&manifest.runtime)?;
    if manifest.grammars.len() != 20 {
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
    let expected_languages = BTreeSet::from([
        "bash",
        "c",
        "cpp",
        "csharp",
        "css",
        "go",
        "html",
        "java",
        "javascript",
        "json",
        "kotlin",
        "lua",
        "php",
        "python",
        "ruby",
        "rust",
        "swift",
        "toml",
        "yaml",
        "typescript",
    ]);
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
    require_nonempty_collection("runtime capabilities", &runtime.capabilities)?;
    require_nonempty_collection("runtime parser_limits", &runtime.parser_limits)
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
        ("analysis_tier", grammar.analysis_tier.as_str()),
        ("semantic_depth", grammar.semantic_depth.as_str()),
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
    if grammar.analysis_tier != "D" {
        return Err(GrammarLockError::InvalidAnalysisTier {
            language: grammar.language.clone(),
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
    if matches!(grammar.language.as_str(), "javascript" | "typescript")
        && (grammar.audit_notes.len() != 2 || grammar.license_source != TYPESCRIPT_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat(
            if grammar.language == "javascript" {
                "javascript"
            } else {
                "typescript"
            },
        ));
    }
    if grammar.language == "cpp"
        && (grammar.audit_notes.len() != 3 || grammar.license_source != CPP_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("cpp"));
    }
    if grammar.language == "kotlin"
        && (grammar.audit_notes.len() != 3 || grammar.license_source != KOTLIN_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("kotlin"));
    }
    if grammar.language == "lua"
        && (grammar.audit_notes.len() != 3 || grammar.license_source != LUA_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("lua"));
    }
    if grammar.language == "ruby"
        && (grammar.audit_notes.len() != 3 || grammar.license_source != RUBY_LICENSE_PATH)
    {
        return Err(GrammarLockError::MissingAuditCaveat("ruby"));
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
        if manifest
            .grammars
            .iter()
            .any(|grammar| grammar.crate_name == name && grammar.vendored.is_some())
        {
            let local = lock
                .package
                .iter()
                .filter(|package| package.name == name && package.version == version)
                .collect::<Vec<_>>();
            if local.len() != 1 || local[0].checksum.is_some() || local[0].source.is_some() {
                return Err(GrammarLockError::CargoLockChecksum {
                    package: name.to_owned(),
                    version: version.to_owned(),
                });
            }
            continue;
        }
        if observed.get(&(name, version)) != Some(&checksum) {
            return Err(GrammarLockError::CargoLockChecksum {
                package: name.to_owned(),
                version: version.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_vendored_sources(
    metadata: &Metadata,
    root: &Path,
    manifest: &GrammarLock,
) -> Result<(), GrammarLockError> {
    for grammar in &manifest.grammars {
        let Some(vendored) = &grammar.vendored else {
            continue;
        };
        let invalid = || GrammarLockError::PackageEvidence {
            package: grammar.crate_name.clone(),
        };
        let manifest_path = Path::new(&vendored.manifest_path);
        if manifest_path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(invalid());
        }
        let expected_manifest = root.join(manifest_path);
        let package = metadata
            .packages
            .iter()
            .find(|package| {
                package.name.as_str() == grammar.crate_name
                    && package.version.to_string() == grammar.crate_version
            })
            .ok_or_else(invalid)?;
        if package.source.is_some() || package.manifest_path.as_std_path() != expected_manifest {
            return Err(invalid());
        }
        let directory = expected_manifest.parent().ok_or_else(invalid)?;
        validate_vendored_tree(directory, grammar, vendored)?;
    }
    Ok(())
}

fn validate_vendored_tree(
    directory: &Path,
    grammar: &GrammarEvidence,
    vendored: &VendoredGrammarEvidence,
) -> Result<(), GrammarLockError> {
    let invalid = || GrammarLockError::PackageEvidence {
        package: grammar.crate_name.clone(),
    };
    let mut observed = BTreeSet::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(path) = pending.pop() {
        let kind = fs::symlink_metadata(&path)
            .map_err(|source| GrammarLockError::Read {
                path: path.clone(),
                source,
            })?
            .file_type();
        if kind.is_symlink() || !kind.is_dir() {
            return Err(invalid());
        }
        let entries = fs::read_dir(&path).map_err(|source| GrammarLockError::Read {
            path: path.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| GrammarLockError::Read {
                path: path.clone(),
                source,
            })?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|source| GrammarLockError::Read {
                path: path.clone(),
                source,
            })?;
            if kind.is_symlink() {
                return Err(invalid());
            }
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !kind.is_file() {
                return Err(invalid());
            }
            let relative = path
                .strip_prefix(directory)
                .map_err(|_| invalid())?
                .to_string_lossy()
                .replace('\\', "/");
            let digest = vendored.files.get(&relative).ok_or_else(invalid)?;
            validate_sha256("vendored source", digest)?;
            let bytes = fs::read(&path).map_err(|source| GrammarLockError::Read {
                path: path.clone(),
                source,
            })?;
            require_digest("vendored grammar source", &bytes, digest)?;
            observed.insert(relative);
        }
    }
    if observed != vendored.files.keys().cloned().collect() || !observed.contains("Cargo.toml") {
        return Err(invalid());
    }
    let scanner = (grammar.scanner_sha256 != "none").then_some(&grammar.scanner_sha256);
    if vendored.files.get("src/parser.c") != Some(&grammar.parser_sha256)
        || vendored.files.get("src/scanner.c") != scanner
        || vendored.files.get("LICENSE") != Some(&grammar.license_sha256)
    {
        return Err(invalid());
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
    parser_limits: Vec<String>,
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
    analysis_tier: String,
    license: String,
    license_source: String,
    license_sha256: String,
    modifications: String,
    test_corpus: String,
    msrv: String,
    offline_behavior: String,
    capabilities: Vec<String>,
    semantic_depth: String,
    audit_notes: Vec<String>,
    #[serde(default)]
    vendored: Option<VendoredGrammarEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VendoredGrammarEvidence {
    manifest_path: String,
    files: BTreeMap<String, String>,
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
    source: Option<String>,
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
    #[error("grammar lock contains {0} grammars instead of twelve")]
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
    #[error("grammar {language} must remain at structural analysis tier D")]
    InvalidAnalysisTier { language: String },
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
    fn checked_in_manifest_matches_the_reviewed_runtime_and_grammar_set() {
        let manifest: GrammarLock = toml::from_str(include_str!("../../adapters/grammars.lock"))
            .expect("grammar lock parses");
        validate_manifest(&manifest).expect("reviewed runtime and grammar identities agree");
    }

    #[test]
    fn vendored_tree_rejects_modified_missing_and_extra_inputs() {
        let (directory, grammar) = vendored_fixture();
        let vendored = grammar
            .vendored
            .as_ref()
            .expect("fixture has local evidence");
        validate_vendored_tree(directory.path(), &grammar, vendored).expect("exact tree passes");
        fs::write(directory.path().join("src/scanner.c"), b"changed").expect("mutation writes");
        assert!(matches!(
            validate_vendored_tree(directory.path(), &grammar, vendored),
            Err(GrammarLockError::DigestMismatch { .. })
        ));
        fs::write(directory.path().join("src/scanner.c"), b"scanner").expect("scanner restores");
        fs::remove_file(directory.path().join("src/parser.c")).expect("fixture parser removes");
        assert!(validate_vendored_tree(directory.path(), &grammar, vendored).is_err());
        fs::write(directory.path().join("src/parser.c"), b"parser").expect("parser restores");
        fs::write(directory.path().join("src/extra.h"), b"extra")
            .expect("unreviewed header writes");
        assert!(validate_vendored_tree(directory.path(), &grammar, vendored).is_err());
    }

    #[test]
    fn vendored_tree_binds_file_hashes_to_the_published_descriptor() {
        let (directory, mut grammar) = vendored_fixture();
        grammar.scanner_sha256 = sha256_hex(b"different descriptor");
        assert!(matches!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().expect("local evidence")
            ),
            Err(GrammarLockError::PackageEvidence { .. })
        ));
    }

    #[test]
    fn scannerless_vendor_requires_both_absent_file_and_absent_descriptor() {
        let (directory, mut grammar) = vendored_fixture();
        grammar.scanner_sha256 = "none".into();
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().expect("evidence")
            )
            .is_err()
        );
        grammar
            .vendored
            .as_mut()
            .expect("evidence")
            .files
            .remove("src/scanner.c");
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().expect("evidence")
            )
            .is_err(),
            "undeclared scanner still exists"
        );
        fs::remove_file(directory.path().join("src/scanner.c")).expect("remove fixture scanner");
        validate_vendored_tree(
            directory.path(),
            &grammar,
            grammar.vendored.as_ref().expect("evidence"),
        )
        .expect("scannerless grammar passes");
        grammar.scanner_sha256 = sha256_hex(b"scanner");
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().expect("evidence")
            )
            .is_err(),
            "declared scanner is missing"
        );
    }

    fn vendored_fixture() -> (tempfile::TempDir, GrammarEvidence) {
        let directory = tempfile::tempdir().expect("fixture directory");
        fs::create_dir(directory.path().join("src")).expect("source directory");
        let mut files = BTreeMap::new();
        for (path, bytes) in [
            ("Cargo.toml", b"manifest".as_slice()),
            ("src/parser.c", b"parser"),
            ("src/scanner.c", b"scanner"),
            ("LICENSE", b"license"),
        ] {
            fs::write(directory.path().join(path), bytes).expect("fixture source writes");
            files.insert(path.to_owned(), sha256_hex(bytes));
        }
        let grammar = GrammarEvidence {
            language: "fixture".into(),
            crate_name: "tree-sitter-fixture".into(),
            crate_version: "1.0.0".into(),
            crates_io_checksum: sha256_hex(b"archive"),
            repository: "https://example.invalid/grammar".into(),
            tag: "v1.0.0".into(),
            commit: "fixture".into(),
            parser_sha256: sha256_hex(b"parser"),
            scanner_sha256: sha256_hex(b"scanner"),
            abi: 15,
            analysis_tier: "D".into(),
            license: "MIT".into(),
            license_source: "LICENSE".into(),
            license_sha256: sha256_hex(b"license"),
            modifications: "fixture".into(),
            test_corpus: "fixture".into(),
            msrv: "1.90".into(),
            offline_behavior: "fixture".into(),
            capabilities: vec!["fixture".into()],
            semantic_depth: "fixture".into(),
            audit_notes: Vec::new(),
            vendored: Some(VendoredGrammarEvidence {
                manifest_path: "third_party/grammar/Cargo.toml".into(),
                files,
            }),
        };
        (directory, grammar)
    }

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
