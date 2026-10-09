//! Verifies the reviewed Tree-sitter source, license, and Cargo dependency lock.
//!
//! The manifest is intentionally redundant with Cargo metadata: policy checks
//! fail if a dependency update changes either executable code or audit evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::cargo_metadata::{Dependency, DependencyKind, Metadata};
use serde::Deserialize;

const GRAMMAR_LOCK_PATH: &str = "adapters/grammars.lock";
const CARGO_LOCK_PATH: &str = "Cargo.lock";
const ADAPTER_PACKAGE: &str = "rootlight-adapter-treesitter";
const GRAMMAR_LOCK_SHA256: &str =
    "61c7d08e950d07cb994be922d62a68a073d328a1e24e46247dda88b7acf10e00";
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

const EXPECTED_PACKAGES: [(&str, &str, &str); 35] = [
    (
        "tree-sitter",
        "0.26.11",
        "af1c71c1c4cc0920b20d6b0f6572e7682cd07a6a2faec71067a31fa394c586df",
    ),
    (
        "dekobon-tree-sitter-groovy",
        "0.2.0",
        "891a2367f48836c854e84bac87f17513707a65a20dce7cab5707ddde6ca23fe0",
    ),
    (
        "ts-parser-perl",
        "2.0.0",
        "db3cd8574afc19af4d3db44fe0cf94a5e8fc3f4056c0326baf5ba01a29666129",
    ),
    (
        "tree-sitter-matlab",
        "1.3.0",
        "8e8d8831a78547f54860dd8467166b1ab0c466d03d43c4bb447af55c024281f7",
    ),
    (
        "tree-sitter-nix",
        "0.3.0",
        "4952a9733f3a98f6683a0ccd1035d84ab7a52f7e84eeed58548d86765ad92de3",
    ),
    (
        "tree-sitter-objc",
        "3.0.2",
        "9ca8bb556423fc176f0535e79d525f783a6684d3c9da81bf9d905303c129e1d2",
    ),
    (
        "tree-sitter-astro-next",
        "0.1.1",
        "794a4a59fc2d88e49b4bc41fef9522d77184a36f4e68bbaf545cd1eb2364c46e",
    ),
    (
        "tree-sitter-md",
        "0.5.3",
        "2efd398be546456c814598ee56c0f51769a77241511b4a58077815d120afa882",
    ),
    (
        "tree-sitter-powershell",
        "0.26.4",
        "3faf304d44b9ddd4a7d97804bb8de7daf564336dd5a526dc6de5b39238243022",
    ),
    (
        "tree-sitter-dart",
        "0.2.0",
        "325dd1e24ee9ee21111e9c43680ae7d6010aaa9f282b048a99b9c7163c1cf553",
    ),
    (
        "tree-sitter-scala",
        "0.26.2",
        "24e0ab4505990bfe30051761d40a7bf4033ce5a81c9eda9e20e987a5cdc84826",
    ),
    (
        "tree-sitter-r",
        "1.3.0",
        "ffc9954ec870dcad6cffdd302b405306c68cf031ed79a78cd9746f6740d9fe20",
    ),
    (
        "tree-sitter-sequel",
        "0.3.11",
        "9d198ad3c319c02e43c21efa1ec796b837afcb96ffaef1a40c1978fbdcec7d17",
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
    (
        "tree-sitter-solidity",
        "1.2.13",
        "4eacf8875b70879f0cb670c60b233ad0b68752d9e1474e6c3ef168eea8a90b25",
    ),
    (
        "tree-sitter-haskell",
        "0.23.1",
        "977c51e504548cba13fc27cb5a2edab2124cf6716a1934915d07ab99523b05a4",
    ),
    (
        "tree-sitter-fortran",
        "0.6.0",
        "ea5fea41363a9b59666001dc26c4b356d4e164da27c6ce31145e00aba6bf9b16",
    ),
];

const EXPECTED_LOCAL_PACKAGES: [(&str, &str, &str, &str); 4] = [
    (
        "tree-sitter-vba",
        "0.14.1",
        "third_party/tree-sitter-vba-0.14.1",
        "058debd1085c94a23989976f4c9289e30bbde00ee5b4e729f8185362ba972560",
    ),
    (
        "tree-sitter-cobol",
        "0.0.1",
        "third_party/tree-sitter-cobol-0.0.1",
        "de05274f23c60ac2cb9a90af56d9eedb6f7aa94e77a1407b87c178202e6eafb8",
    ),
    (
        "tree-sitter-plsql",
        "0.0.1",
        "third_party/tree-sitter-plsql-0.0.1",
        "7d4adf5f85fb5c0957b0c54a88ebc8a4a36bafb343535ad696a23aa5454b66fd",
    ),
    (
        "tree-sitter-fsharp",
        "0.3.12",
        "third_party/tree-sitter-fsharp-0.3.12",
        "813a911d6caba550be7af1b369c15df347f80a07eab8f3720ea9da476693b37a",
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
    validate_direct_dependencies(metadata, root)?;
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
        "adapters/licenses/dekobon-tree-sitter-groovy-0.2.0-LICENSE-MIT",
        "7846ff7803888ebb32f936a8a858eb9d5d73560401d92a2f358d562642205263",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/dekobon-tree-sitter-groovy-0.2.0-LICENSE-APACHE",
        "c95bae1d1ce0235ecccd3560b772ec1efb97f348a79f0fbe0a634f0c2ccefe2c",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/ts-parser-perl-2.0.0-LICENSE",
        "68a9a526ae357ed5a2f8ca5dabb14131b283554b69639bb04816088d4b1f2fa0",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-matlab-1.3.0-LICENSE",
        "8673f5acd59bfb3bd129760a57f72092d5e3c01e05837532eac9497cf8006a46",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-nix-0.3.0-LICENSE",
        "c1b72e50266464ec5118d26200e17bf1f472d142e62ab637e1ffc23986657a78",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-objc-3.0.2-LICENSE",
        "099c44248f8cf353123211318680e93465587c005a4b3730ce8cb5334de043d6",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-objc-3.0.2-LICENSE-C",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-astro-next-0.1.1-LICENSE",
        "62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-astro-next-0.1.1-LICENSE-MIT",
        "9ec95a90150fa19e2cee78faf49138fc480f6f7a2354006a0ab150c44239447e",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-md-0.5.3-LICENSE",
        "52ec8a1bf8256511e2a92613e0bb41ffefba3897f16fa958d7bb28c466dd4804",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-powershell-0.26.4-LICENSE",
        "1fca8454c0d77a73f9922a52d39e1f7c339f5da5d03f2766eff66b859fa19477",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-r-1.3.0-LICENSE",
        "de2e49529f03d573bc3fa229dc83acfe22c63a5ad1766b289563edfd45b72dea",
    )?;
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-sequel-0.3.11-LICENSE",
        "3b3e4e4252d5d7d1c18e1257005f23242bf0580ad619204fd093c99b8c56748d",
    )?;
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
    validate_local_license(
        root,
        "adapters/licenses/tree-sitter-haskell-0.23.1-LICENSE",
        "2e0110e07abef7c2548b26ec9d6969775617ca539a0dc8dbeeb14d6452c711d1",
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
    if manifest.grammars.len() != 41 {
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
        let identity = (
            grammar.crate_version.as_str(),
            grammar.crates_io_checksum.as_deref(),
        );
        if let Some(previous) = packages.insert(grammar.crate_name.as_str(), identity)
            && previous != identity
        {
            return Err(GrammarLockError::PackageEvidence {
                package: grammar.crate_name.clone(),
            });
        }
    }
    let expected_languages = BTreeSet::from([
        "astro",
        "bash",
        "c",
        "cobol-fixed",
        "cobol-free",
        "cpp",
        "csharp",
        "css",
        "dart",
        "fsharp",
        "fsharp-signature",
        "fortran",
        "go",
        "groovy",
        "haskell",
        "html",
        "sql",
        "java",
        "javascript",
        "json",
        "kotlin",
        "lua",
        "markdown",
        "matlab",
        "nix",
        "objective-c",
        "perl",
        "plsql",
        "php",
        "powershell",
        "python",
        "r",
        "solidity",
        "scala",
        "ruby",
        "rust",
        "swift",
        "toml",
        "yaml",
        "typescript",
        "vba",
    ]);
    if languages != expected_languages {
        return Err(GrammarLockError::LanguageSet);
    }
    for (name, version, checksum) in EXPECTED_PACKAGES.iter().skip(1) {
        if packages.get(name) != Some(&(*version, Some(*checksum))) {
            return Err(GrammarLockError::PackageEvidence {
                package: (*name).to_owned(),
            });
        }
    }
    for (name, version, directory, checksum) in EXPECTED_LOCAL_PACKAGES {
        let grammars: Vec<_> = manifest
            .grammars
            .iter()
            .filter(|grammar| grammar.crate_name == name)
            .collect();
        if grammars.is_empty() {
            return Err(GrammarLockError::PackageEvidence {
                package: name.to_owned(),
            });
        }
        // Multi-profile bindings must retain one complete source identity for
        // every language record, not only the first matching package entry.
        for grammar in grammars {
            if grammar.crate_version != version
                || grammar.crates_io_checksum.is_some()
                || grammar.vendored_source_sha256.as_deref() != Some(checksum)
                || grammar
                    .vendored
                    .as_ref()
                    .map(|vendored| vendored.manifest_path.as_str())
                    != Some(format!("{directory}/Cargo.toml").as_str())
            {
                return Err(GrammarLockError::PackageEvidence {
                    package: name.to_owned(),
                });
            }
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
    validate_source_provenance(grammar)?;
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

fn validate_source_provenance(grammar: &GrammarEvidence) -> Result<(), GrammarLockError> {
    match (
        grammar.crates_io_checksum.as_deref(),
        grammar.vendored_source_sha256.as_deref(),
        grammar.vendored.as_ref(),
    ) {
        (Some(checksum), None, _) => validate_sha256("grammar crate checksum", checksum),
        (None, Some(checksum), Some(vendored)) => {
            validate_sha256("vendored source manifest", checksum)?;
            let observed = vendored_source_digest(&vendored.files)?;
            if observed != checksum {
                return Err(GrammarLockError::DigestMismatch {
                    label: "vendored source manifest",
                    expected: checksum.to_owned(),
                    observed,
                });
            }
            Ok(())
        }
        _ => Err(GrammarLockError::SourceProvenance {
            package: grammar.crate_name.clone(),
        }),
    }
}

// A local binding need not have a published archive. Bind its source identity
// to sorted UTF-8 paths and lowercase file hashes instead: domain prefix, then
// path, NUL, hash, NUL for each entry. NUL-free paths make framing unambiguous.
fn vendored_source_digest(files: &BTreeMap<String, String>) -> Result<String, GrammarLockError> {
    let mut digest = rootlight_crypto::Sha256::new();
    digest.update(b"rootlight-vendored-source-v1\0");
    for (path, checksum) in files {
        if !is_source_relative_path(path) {
            return Err(GrammarLockError::InvalidSourcePath(path.clone()));
        }
        validate_sha256("vendored source", checksum)?;
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(checksum.as_bytes());
        digest.update([0]);
    }
    Ok(rootlight_encoding::HEXLOWER.encode(&digest.finalize()))
}

fn is_source_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.split('/').any(|part| {
            part.is_empty() || matches!(part, "." | "..") || part.contains(['\\', ':', '\0'])
        })
}

fn validate_direct_dependencies(metadata: &Metadata, root: &Path) -> Result<(), GrammarLockError> {
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == ADAPTER_PACKAGE)
        .ok_or(GrammarLockError::MissingAdapterPackage)?;
    validate_tree_sitter_dependencies(&package.dependencies, root)
}

fn validate_tree_sitter_dependencies(
    dependencies: &[Dependency],
    root: &Path,
) -> Result<(), GrammarLockError> {
    let expected: BTreeMap<_, _> = EXPECTED_PACKAGES
        .iter()
        .map(|(name, version, _)| (*name, (*version, None)))
        .chain(
            EXPECTED_LOCAL_PACKAGES
                .iter()
                .map(|(name, version, directory, _)| {
                    (*name, (*version, Some(root.join(directory))))
                }),
        )
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
    for (name, (version, path)) in expected {
        let dependency = observed
            .iter()
            .find(|dependency| dependency.name == name)
            .ok_or(GrammarLockError::DependencySet)?;
        validate_dependency_profile(dependency, version, path.as_deref())?;
    }
    Ok(())
}

fn is_tree_sitter_package(name: &str) -> bool {
    name == "tree-sitter"
        || name == "dekobon-tree-sitter-groovy"
        || name.starts_with("tree-sitter-")
        || name.starts_with("ts-parser-")
}

fn validate_dependency_profile(
    dependency: &Dependency,
    version: &str,
    expected_path: Option<&Path>,
) -> Result<(), GrammarLockError> {
    let source_matches = match expected_path {
        Some(path) => dependency.source.is_none() && dependency.path.as_deref() == Some(path),
        None => {
            dependency.path.is_none()
                && dependency
                    .source
                    .as_ref()
                    .map(ToString::to_string)
                    .as_deref()
                    == Some(CRATES_IO_SOURCE)
        }
    };
    if dependency.req != format!("={version}")
        || dependency.kind != DependencyKind::Normal
        || dependency.optional
        || dependency.uses_default_features
        || dependency.rename.is_some()
        || dependency.target.is_some()
        || dependency.registry.is_some()
        || !source_matches
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
    validate_locked_packages(&lock, manifest)
}

fn validate_locked_packages(
    lock: &CargoLock,
    manifest: &GrammarLock,
) -> Result<(), GrammarLockError> {
    let mut expected = vec![(
        manifest.runtime.crate_name.as_str(),
        manifest.runtime.crate_version.as_str(),
        Some(manifest.runtime.crates_io_checksum.as_str()),
        false,
    )];
    for grammar in &manifest.grammars {
        validate_source_provenance(grammar)?;
        expected.push((
            grammar.crate_name.as_str(),
            grammar.crate_version.as_str(),
            grammar.crates_io_checksum.as_deref(),
            grammar.vendored.is_some(),
        ));
    }
    for (name, version, checksum, vendored) in expected {
        let entries = lock
            .package
            .iter()
            .filter(|package| package.name == name && package.version == version)
            .collect::<Vec<_>>();
        let valid = match entries.as_slice() {
            [package] if vendored => package.checksum.is_none() && package.source.is_none(),
            [package] => {
                checksum.is_some()
                    && package.checksum.as_deref() == checksum
                    && package.source.as_deref() == Some(CRATES_IO_SOURCE)
            }
            _ => false,
        };
        if !valid {
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
                    && package.version == grammar.crate_version
            })
            .ok_or_else(invalid)?;
        if package.source.is_some() || package.manifest_path != expected_manifest {
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
    validate_source_provenance(grammar)?;
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
    // Explicit source paths accommodate multi-grammar packages without accepting
    // a digest from an unrelated file or resolving a path outside the package.
    let parser_path = vendored.parser_path.as_deref().unwrap_or("src/parser.c");
    let scanner_path = vendored.scanner_path.as_deref().unwrap_or("src/scanner.c");
    let license_path = vendored.license_path.as_deref().unwrap_or("LICENSE");
    for path in [parser_path, scanner_path, license_path] {
        if !is_source_relative_path(path) {
            return Err(invalid());
        }
    }
    if vendored.files.get(parser_path) != Some(&grammar.parser_sha256)
        || vendored.files.get(scanner_path) != scanner
        || vendored.files.get(license_path) != Some(&grammar.license_sha256)
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

    let digest = rootlight_crypto::Sha256::digest(bytes);
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
    crates_io_checksum: Option<String>,
    vendored_source_sha256: Option<String>,
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
    #[serde(default)]
    parser_path: Option<String>,
    #[serde(default)]
    scanner_path: Option<String>,
    #[serde(default)]
    license_path: Option<String>,
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
    #[error("grammar lock contains {0} grammars instead of 41")]
    GrammarCount(usize),
    #[error("grammar lock repeats language {0}")]
    DuplicateLanguage(String),
    #[error("grammar lock does not contain the exact audited language set")]
    LanguageSet,
    #[error("runtime evidence differs from the audited Tree-sitter release")]
    RuntimeEvidence,
    #[error("package evidence differs for {package}")]
    PackageEvidence { package: String },
    #[error("grammar {package} requires exactly one archive or vendored source identity")]
    SourceProvenance { package: String },
    #[error("vendored source path is not a canonical relative path: {0:?}")]
    InvalidSourcePath(String),
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
        require_digest(
            GRAMMAR_LOCK_PATH,
            include_bytes!("../../adapters/grammars.lock"),
            GRAMMAR_LOCK_SHA256,
        )
        .expect("grammar lock digest matches the reviewed pin");
        let manifest: GrammarLock = toml::from_str(include_str!("../../adapters/grammars.lock"))
            .expect("grammar lock parses");
        validate_manifest(&manifest).expect("reviewed runtime and grammar identities agree");
    }

    #[test]
    fn local_multi_profile_records_require_one_reviewed_source_tree() {
        for (language, package, review_path) in [
            ("cobol-fixed", "tree-sitter-cobol", "PATCHES.md"),
            ("cobol-free", "tree-sitter-cobol", "PATCHES.md"),
            ("fsharp", "tree-sitter-fsharp", "ROOTLIGHT-PATCH.md"),
            (
                "fsharp-signature",
                "tree-sitter-fsharp",
                "ROOTLIGHT-PATCH.md",
            ),
        ] {
            let mut manifest: GrammarLock =
                toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
            let grammar = manifest
                .grammars
                .iter_mut()
                .find(|grammar| grammar.language == language)
                .unwrap();
            let files = &mut grammar.vendored.as_mut().unwrap().files;
            files.insert(review_path.into(), sha256_hex(b"different source review"));
            grammar.vendored_source_sha256 = Some(vendored_source_digest(files).unwrap());
            validate_grammar(grammar)
                .expect("self-consistent manifest is not the reviewed source pin");
            assert!(matches!(validate_manifest(&manifest),
                Err(GrammarLockError::PackageEvidence { package: observed }) if observed == package));
        }
    }

    #[test]
    fn fsharp_profiles_authenticate_shared_sources_and_distinct_parser_paths() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let implementation = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "fsharp")
            .unwrap();
        let signature = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "fsharp-signature")
            .unwrap();
        let implementation_sources = implementation.vendored.as_ref().unwrap();
        let signature_sources = signature.vendored.as_ref().unwrap();
        assert_eq!(implementation_sources.files, signature_sources.files);
        assert_eq!(implementation_sources.files.len(), 33);
        assert_eq!(
            implementation_sources.parser_path.as_deref(),
            Some("fsharp/src/parser.c")
        );
        assert_eq!(
            signature_sources.parser_path.as_deref(),
            Some("fsharp_signature/src/parser.c")
        );
        assert_ne!(implementation.parser_sha256, signature.parser_sha256);
        for grammar in [implementation, signature] {
            let sources = grammar.vendored.as_ref().unwrap();
            assert_eq!(sources.scanner_path.as_deref(), Some("common/scanner.h"));
            assert_eq!(
                sources.files.get("common/scanner.h"),
                Some(&grammar.scanner_sha256)
            );
            for path in [
                "common/scanner.h.orig",
                "fsharp/grammar.js.orig",
                "fsharp_signature/grammar.js.orig",
                "fsharp/src/scanner.c",
                "fsharp_signature/src/scanner.c",
                "SOURCE-PROVENANCE.json",
                "ROOTLIGHT-PATCH.md",
                "LICENSE",
            ] {
                assert!(
                    sources.files.contains_key(path),
                    "missing retained input: {path}"
                );
            }
            validate_vendored_tree(
                &root.join("third_party/tree-sitter-fsharp-0.3.12"),
                grammar,
                sources,
            )
            .expect("both profiles authenticate the complete reviewed source tree");
        }
    }

    #[test]
    fn fortran_retains_original_inputs_and_declares_scanner_patch() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "fortran")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        for (path, expected) in [
            (
                "src/scanner.c.orig",
                "84487995be2bdfeba996baa7c983affb8dd33d0c38faeaadaab7a4b1fda9aa7c",
            ),
            (
                "upstream/tree-sitter-fortran-0.6.0.crate",
                "ea5fea41363a9b59666001dc26c4b356d4e164da27c6ce31145e00aba6bf9b16",
            ),
            (
                "upstream/tree-sitter-fortran-48593829df8a929bfe58528eedcf405171921005.tar.gz",
                "e7f87ad365f254b4048b7a3655407ea669382ff2b4ae7cfd3f5cbdabab16dacd",
            ),
        ] {
            assert_eq!(sources.files.get(path).map(String::as_str), Some(expected));
        }
        for path in [
            "grammar.js",
            "src/node-types.json",
            "LICENSE",
            "PATCHES.md",
            "SOURCE-PROVENANCE.json",
        ] {
            assert!(
                sources.files.contains_key(path),
                "missing retained input: {path}"
            );
        }
        assert_ne!(
            sources.files["src/scanner.c"],
            sources.files["src/scanner.c.orig"]
        );
        validate_vendored_tree(
            &root.join("third_party/tree-sitter-fortran-0.6.0"),
            grammar,
            sources,
        )
        .expect("the complete authored tree and local safety patch are authenticated");
    }

    #[test]
    fn plsql_retains_original_inputs_and_authenticates_local_generation() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let manifest: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        let grammar = manifest
            .grammars
            .iter()
            .find(|grammar| grammar.language == "plsql")
            .unwrap();
        let sources = grammar.vendored.as_ref().unwrap();
        assert!(grammar.crates_io_checksum.is_none());
        assert_eq!(grammar.abi, 14);
        assert_eq!(grammar.scanner_sha256, "none");
        assert_eq!(sources.files.len(), 39);
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../third_party/tree-sitter-plsql-0.0.1/SOURCE-PROVENANCE.json"
        ))
        .unwrap();
        let originals = provenance["upstream_original_files"].as_array().unwrap();
        assert_eq!(originals.len(), 27);
        for original in originals {
            let retained = original["retained_path"].as_str().unwrap();
            assert_eq!(
                sources.files.get(retained).map(String::as_str),
                original["sha256"].as_str(),
                "retained original source differs: {retained}"
            );
        }
        let changes = provenance["source_modifications"].as_array().unwrap();
        assert_eq!(changes.len(), 8);
        for change in changes {
            let path = change["path"].as_str().unwrap();
            let original = change["original_path"].as_str().unwrap();
            assert_eq!(
                sources.files.get(path).map(String::as_str),
                change["local_sha256"].as_str()
            );
            assert_eq!(
                sources.files.get(original).map(String::as_str),
                change["original_sha256"].as_str()
            );
            assert_ne!(sources.files[path], sources.files[original]);
            assert_eq!(
                change["patch_document_sha256"].as_str(),
                sources.files.get("PATCHES.md").map(String::as_str)
            );
        }
        assert_eq!(
            provenance["original_generated_parser_unchanged"],
            serde_json::Value::Bool(false)
        );
        assert_eq!(
            provenance["preferred_grammar_unchanged"],
            serde_json::Value::Bool(false)
        );
        assert_eq!(provenance["generated_parser_language_abi"], 14);
        assert_eq!(provenance["generated_parser_state_count"], 10577);
        assert_eq!(provenance["generated_parser_external_token_count"], 0);
        let generation = &provenance["local_generation"];
        assert_eq!(generation["version"], "0.24.4");
        assert_eq!(generation["javascript_runtime"], "Node.js 24.11.1");
        assert_eq!(
            generation["preferred_input_sha256"].as_str(),
            sources.files.get("grammar.js").map(String::as_str)
        );
        assert_eq!(generation["independent_regeneration_count"], 2);
        assert_eq!(
            generation["independent_regeneration_exit_codes"],
            serde_json::json!([0, 0])
        );
        assert_eq!(
            generation["independent_regeneration_stderr_bytes"],
            serde_json::json!([0, 0])
        );
        assert_eq!(
            generation["independent_regenerations_byte_identical"],
            serde_json::Value::Bool(true)
        );
        let outputs = generation["generated_output_sha256"].as_object().unwrap();
        assert_eq!(outputs.len(), 6);
        for (path, checksum) in outputs {
            assert_eq!(
                checksum.as_str(),
                sources.files.get(path).map(String::as_str),
                "generated source differs: {path}"
            );
        }
        let generated_originals = generation["original_generated_copies"].as_array().unwrap();
        assert_eq!(generated_originals.len(), 4);
        for original in generated_originals {
            let path = original.as_str().unwrap();
            assert!(originals.iter().any(|input| input["retained_path"] == path));
        }
        let headers = generation["new_generator_headers"].as_array().unwrap();
        assert_eq!(headers.len(), 2);
        for header in headers {
            let path = header.as_str().unwrap();
            assert!(outputs.contains_key(path));
            assert!(originals.iter().all(|input| input["path"] != path));
        }
        for claim in [
            "binary_signature_independently_verified",
            "upstream_original_tables_reproduced",
            "all_tree_behavioral_equivalence_proven",
            "os_network_denial_verified",
        ] {
            assert_eq!(generation[claim], serde_json::Value::Bool(false));
        }
        for claim in [
            "native_parser_execution_verified",
            "source_to_binary_reproduction_verified",
            "full_plsql_conformance_proven",
            "build_and_packaging_acceptance_verified",
        ] {
            assert_eq!(provenance[claim], serde_json::Value::Bool(false));
        }
        validate_vendored_tree(
            &root.join("third_party/tree-sitter-plsql-0.0.1"),
            grammar,
            sources,
        )
        .expect(
            "the original upstream tree, local grammar and generated sources are authenticated",
        );
    }

    #[test]
    fn local_source_identity_requires_exactly_one_provenance_and_a_live_tree() {
        let (directory, mut grammar) = vendored_fixture();
        grammar.crates_io_checksum = None;
        assert!(matches!(
            validate_grammar(&grammar),
            Err(GrammarLockError::SourceProvenance { .. })
        ));
        grammar.vendored_source_sha256 =
            Some(vendored_source_digest(&grammar.vendored.as_ref().unwrap().files).unwrap());
        validate_grammar(&grammar).unwrap();
        validate_vendored_tree(
            directory.path(),
            &grammar,
            grammar.vendored.as_ref().unwrap(),
        )
        .unwrap();
        grammar.crates_io_checksum = Some(sha256_hex(b"archive"));
        assert!(matches!(
            validate_grammar(&grammar),
            Err(GrammarLockError::SourceProvenance { .. })
        ));
        grammar.crates_io_checksum = None;
        grammar.vendored = None;
        assert!(matches!(
            validate_grammar(&grammar),
            Err(GrammarLockError::SourceProvenance { .. })
        ));
    }

    #[test]
    fn source_manifest_digest_binds_paths_and_content_with_portable_framing() {
        let files = BTreeMap::from([("src/λ.bas".into(), sha256_hex(b"source"))]);
        // Independently computed from the documented UTF-8/NUL format, not TOML
        // serialization or host-native path separators.
        assert_eq!(
            vendored_source_digest(&files).unwrap(),
            "3cc9525c80eb1d734a3e307c9546bfc694f23a0f61fbfeef1d0a93c7d31d34a7"
        );
        let digest = vendored_source_digest(&files).unwrap();
        for changed in [
            BTreeMap::new(),
            BTreeMap::from([("src/other.bas".into(), sha256_hex(b"source"))]),
            BTreeMap::from([("src/λ.bas".into(), sha256_hex(b"changed"))]),
        ] {
            assert_ne!(vendored_source_digest(&changed).unwrap(), digest);
        }
        for path in [
            "", "/src/a", "../a", "src/./a", "src//a", "src\\a", "C:/a", "a\0b",
        ] {
            assert!(matches!(
                vendored_source_digest(&BTreeMap::from([(path.into(), sha256_hex(b"source"))])),
                Err(GrammarLockError::InvalidSourcePath(_))
            ));
        }
        assert!(vendored_source_digest(&BTreeMap::from([("a".into(), "invalid".into())])).is_err());
    }

    #[test]
    fn local_source_identity_rejects_rewritten_manifest_and_live_file_changes() {
        let (directory, mut grammar) = vendored_fixture();
        grammar.crates_io_checksum = None;
        let vendored = grammar.vendored.as_mut().unwrap();
        grammar.vendored_source_sha256 = Some(vendored_source_digest(&vendored.files).unwrap());
        let original = vendored
            .files
            .insert("LICENSE".into(), sha256_hex(b"changed"));
        assert!(matches!(
            validate_grammar(&grammar),
            Err(GrammarLockError::DigestMismatch {
                label: "vendored source manifest",
                ..
            })
        ));
        grammar
            .vendored
            .as_mut()
            .unwrap()
            .files
            .insert("LICENSE".into(), original.unwrap());
        fs::write(directory.path().join("LICENSE"), b"changed").unwrap();
        assert!(matches!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            ),
            Err(GrammarLockError::DigestMismatch {
                label: "vendored grammar source",
                ..
            })
        ));
        fs::write(directory.path().join("LICENSE"), b"license").unwrap();
        fs::remove_file(directory.path().join("src/parser.c")).unwrap();
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            )
            .is_err()
        );
        fs::write(directory.path().join("src/parser.c"), b"parser").unwrap();
        fs::write(directory.path().join("extra.h"), b"extra").unwrap();
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn registry_admission_cannot_be_replaced_by_a_local_source_identity() {
        let baseline: GrammarLock =
            toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
        for (index, grammar) in baseline.grammars.iter().enumerate() {
            if grammar.vendored.is_none() || grammar.crates_io_checksum.is_none() {
                continue;
            }
            let mut manifest: GrammarLock =
                toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
            let grammar = &mut manifest.grammars[index];
            grammar.crates_io_checksum = None;
            grammar.vendored_source_sha256 =
                Some(vendored_source_digest(&grammar.vendored.as_ref().unwrap().files).unwrap());
            validate_grammar(grammar).unwrap();
            assert!(
                matches!(
                    validate_manifest(&manifest),
                    Err(GrammarLockError::PackageEvidence { .. })
                ),
                "grammar index {index}"
            );
        }
    }

    #[test]
    fn cargo_lock_requires_one_exact_source_for_registry_and_local_grammars() {
        for vendored in [false, true] {
            for mutation in [
                "none",
                "missing",
                "duplicate",
                "source",
                "checksum",
                "version",
            ] {
                let (_directory, mut grammar) = vendored_fixture();
                if vendored {
                    grammar.crates_io_checksum = None;
                    grammar.vendored_source_sha256 = Some(
                        vendored_source_digest(&grammar.vendored.as_ref().unwrap().files).unwrap(),
                    );
                } else {
                    grammar.vendored = None;
                }
                let mut manifest: GrammarLock =
                    toml::from_str(include_str!("../../adapters/grammars.lock")).unwrap();
                let mut lock: CargoLock = toml::from_str(include_str!("../../Cargo.lock")).unwrap();
                let package = || LockedPackage {
                    name: grammar.crate_name.clone(),
                    version: grammar.crate_version.clone(),
                    checksum: grammar.crates_io_checksum.clone(),
                    source: (!vendored).then(|| CRATES_IO_SOURCE.to_owned()),
                };
                if mutation != "missing" {
                    lock.package.push(package());
                }
                if mutation == "duplicate" {
                    lock.package.push(package());
                }
                let last = lock.package.last_mut().unwrap();
                match mutation {
                    "source" => last.source = Some("registry+https://example.invalid/index".into()),
                    "checksum" => last.checksum = Some(sha256_hex(b"unexpected")),
                    "version" => last.version = "2.0.0".into(),
                    _ => {}
                }
                manifest.grammars = vec![grammar];
                let result = validate_locked_packages(&lock, &manifest);
                if mutation == "none" {
                    result.unwrap();
                } else {
                    assert!(
                        matches!(result, Err(GrammarLockError::CargoLockChecksum { .. })),
                        "vendored={vendored}, mutation={mutation}"
                    );
                }
            }
        }
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
    fn vendored_tree_accepts_explicit_nested_sources_but_not_path_aliases() {
        let (directory, mut grammar) = vendored_fixture();
        fs::create_dir(directory.path().join("block")).unwrap();
        fs::rename(
            directory.path().join("src"),
            directory.path().join("block/src"),
        )
        .unwrap();
        let vendored = grammar.vendored.as_mut().unwrap();
        for name in ["parser", "scanner"] {
            let digest = vendored.files.remove(&format!("src/{name}.c")).unwrap();
            vendored.files.insert(format!("block/src/{name}.c"), digest);
        }
        vendored.parser_path = Some("block/src/parser.c".into());
        vendored.scanner_path = Some("block/src/scanner.c".into());
        validate_vendored_tree(
            directory.path(),
            &grammar,
            grammar.vendored.as_ref().unwrap(),
        )
        .unwrap();
        for path in [
            "",
            "../block/src/parser.c",
            "/block/src/parser.c",
            "block/./src/parser.c",
            "block//src/parser.c",
            "block\\src\\parser.c",
            "C:/block/src/parser.c",
            "block/src/scanner.c",
        ] {
            grammar.vendored.as_mut().unwrap().parser_path = Some(path.into());
            assert!(
                validate_vendored_tree(
                    directory.path(),
                    &grammar,
                    grammar.vendored.as_ref().unwrap()
                )
                .is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn vendored_license_path_requires_exact_file_digest_and_relative_identity() {
        let (directory, mut grammar) = vendored_fixture();
        fs::rename(
            directory.path().join("LICENSE"),
            directory.path().join("LICENSE-MIT"),
        )
        .unwrap();
        let vendored = grammar.vendored.as_mut().unwrap();
        let digest = vendored.files.remove("LICENSE").unwrap();
        vendored.files.insert("LICENSE-MIT".into(), digest);
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            )
            .is_err()
        );
        grammar.vendored.as_mut().unwrap().license_path = Some("LICENSE-MIT".into());
        validate_vendored_tree(
            directory.path(),
            &grammar,
            grammar.vendored.as_ref().unwrap(),
        )
        .unwrap();
        for path in [
            "",
            "../LICENSE-MIT",
            "/LICENSE-MIT",
            "./LICENSE-MIT",
            "C:/LICENSE-MIT",
            "LICENSE",
            "src/parser.c",
        ] {
            grammar.vendored.as_mut().unwrap().license_path = Some(path.into());
            assert!(
                validate_vendored_tree(
                    directory.path(),
                    &grammar,
                    grammar.vendored.as_ref().unwrap()
                )
                .is_err(),
                "{path}"
            );
        }
        grammar.vendored.as_mut().unwrap().license_path = Some("LICENSE-MIT".into());
        grammar.license_sha256 = sha256_hex(b"different license");
        assert!(
            validate_vendored_tree(
                directory.path(),
                &grammar,
                grammar.vendored.as_ref().unwrap()
            )
            .is_err()
        );
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
            crates_io_checksum: Some(sha256_hex(b"archive")),
            vendored_source_sha256: None,
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
                parser_path: None,
                scanner_path: None,
                license_path: None,
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
            validate_tree_sitter_dependencies(&dependencies, Path::new("/workspace")),
            Err(GrammarLockError::DependencySet)
        ));
    }

    #[test]
    fn dependency_profile_rejects_non_normal_optional_and_non_crates_io_forms() {
        let baseline = dependency_fixture("tree-sitter-rust", "0.24.2", &[]);
        let mut profiles = Vec::new();

        let mut development = baseline.clone();
        development.kind = DependencyKind::Development;
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
                validate_dependency_profile(&dependency, "0.24.2", None),
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
            .chain(
                EXPECTED_LOCAL_PACKAGES
                    .iter()
                    .map(|(name, version, directory, _)| {
                        let mut dependency =
                            dependency_fixture_with_source(name, version, &[], None, None);
                        dependency.path = Some(format!("/workspace/{directory}").into());
                        dependency
                    }),
            )
            .collect()
    }

    #[test]
    fn local_dependency_requires_the_reviewed_path_and_exact_profile() {
        let dependencies = audited_dependency_fixtures();
        validate_tree_sitter_dependencies(&dependencies, Path::new("/workspace")).unwrap();
        let baseline = dependencies
            .iter()
            .find(|dependency| dependency.name == "tree-sitter-vba")
            .unwrap();
        let path = Path::new("/workspace/third_party/tree-sitter-vba-0.14.1");
        for mutation in [
            "missing_path",
            "different_path",
            "registry",
            "optional",
            "features",
            "version",
            "renamed",
            "target",
            "development",
            "default_features",
        ] {
            let mut dependency = baseline.clone();
            match mutation {
                "missing_path" => dependency.path = None,
                "different_path" => dependency.path = Some("/workspace/unreviewed".into()),
                "registry" => dependency = dependency_fixture("tree-sitter-vba", "0.14.1", &[]),
                "optional" => dependency.optional = true,
                "features" => dependency.features.push("unreviewed".into()),
                "version" => dependency.req = ">=0.14.1".parse().unwrap(),
                "renamed" => dependency.rename = Some("replacement".into()),
                "target" => dependency.target = Some("cfg(windows)".parse().unwrap()),
                "development" => dependency.kind = DependencyKind::Development,
                "default_features" => dependency.uses_default_features = true,
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    validate_dependency_profile(&dependency, "0.14.1", Some(path)),
                    Err(GrammarLockError::DependencyProfile { .. })
                ),
                "{mutation}"
            );
        }
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
