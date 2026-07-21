//! Verifies the repository license declaration is complete and consistent.
//!
//! The root `LICENSE` text, the README declaration, and the resolved Cargo
//! package metadata must all agree on one SPDX identifier so the public
//! licensing boundary cannot drift silently between prose and manifests.

use std::{fs, path::Path};

use cargo_metadata::{Metadata, MetadataCommand};
use sha2::Digest as _;

const LICENSE_PATH: &str = "LICENSE";
const README_PATH: &str = "README.md";
const EXPECTED_SPDX: &str = "AGPL-3.0-only";
const README_LICENSE_LINK: &str = "./LICENSE";
/// SHA-256 of the canonical GNU AGPL v3 text published at
/// `https://www.gnu.org/licenses/agpl-3.0.txt`, the verbatim license body
/// including the FSF permission notice that repositories bundle as `LICENSE`.
const LICENSE_SHA256: &str = "0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0";

pub(crate) fn check() -> Result<(), LicenseError> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .map_err(LicenseError::Metadata)?;
    let root = metadata.workspace_root.as_std_path();

    let license_bytes = read_required(root, LICENSE_PATH)?;
    require_license_digest(&license_bytes)?;

    let readme_bytes = read_required(root, README_PATH)?;
    let readme_text = std::str::from_utf8(&readme_bytes).map_err(|source| LicenseError::Utf8 {
        relative: README_PATH,
        source,
    })?;
    require_readme_declaration(readme_text)?;

    require_package_licenses(&metadata)?;

    println!(
        "license check passed: {EXPECTED_SPDX} consistent across LICENSE, README, and {} workspace packages",
        metadata.packages.len()
    );
    Ok(())
}

fn read_required(root: &Path, relative: &'static str) -> Result<Vec<u8>, LicenseError> {
    let path = root.join(relative);
    fs::read(&path).map_err(|source| LicenseError::Read { relative, source })
}

fn require_license_digest(bytes: &[u8]) -> Result<(), LicenseError> {
    let observed = sha256_hex(bytes);
    if observed == LICENSE_SHA256 {
        Ok(())
    } else {
        Err(LicenseError::DigestMismatch {
            expected: LICENSE_SHA256.to_owned(),
            observed,
        })
    }
}

fn require_readme_declaration(text: &str) -> Result<(), LicenseError> {
    if !text.contains(EXPECTED_SPDX) {
        return Err(LicenseError::ReadmeDeclaration {
            reason: format!("missing SPDX identifier {EXPECTED_SPDX}"),
        });
    }
    if !text.contains(README_LICENSE_LINK) {
        return Err(LicenseError::ReadmeDeclaration {
            reason: format!("missing link to {LICENSE_PATH}"),
        });
    }
    Ok(())
}

fn require_package_licenses(metadata: &Metadata) -> Result<(), LicenseError> {
    let mut offenders: Vec<String> = metadata
        .packages
        .iter()
        .filter_map(|package| package_license_offender(&package.name, package.license.as_deref()))
        .collect();
    offenders.sort();
    if offenders.is_empty() {
        Ok(())
    } else {
        Err(LicenseError::PackageLicenses {
            expected: EXPECTED_SPDX.to_owned(),
            offenders: offenders.join(", "),
        })
    }
}

/// Returns a diagnostic for a package whose declared license is not the expected
/// SPDX identifier, or `None` when the package conforms.
fn package_license_offender(name: &str, declared: Option<&str>) -> Option<String> {
    if declared == Some(EXPECTED_SPDX) {
        None
    } else {
        Some(format!("{name} ({})", declared.unwrap_or("none")))
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum LicenseError {
    #[error("failed to read cargo metadata")]
    Metadata(#[source] cargo_metadata::Error),
    #[error("failed to read {relative}")]
    Read {
        relative: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("{relative} is not valid UTF-8")]
    Utf8 {
        relative: &'static str,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error(
        "LICENSE checksum mismatch: expected canonical AGPL-3.0-only text sha256 {expected}, found {observed}"
    )]
    DigestMismatch { expected: String, observed: String },
    #[error("README license declaration is incomplete: {reason}")]
    ReadmeDeclaration { reason: String },
    #[error("workspace packages must declare license {expected}: {offenders}")]
    PackageLicenses { expected: String, offenders: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readme_declaration_accepts_valid_text() {
        let text = "Rootlight is licensed under [AGPL](./LICENSE) (`AGPL-3.0-only`).";
        assert!(require_readme_declaration(text).is_ok());
    }

    #[test]
    fn readme_declaration_rejects_missing_spdx() {
        let error = require_readme_declaration("see ./LICENSE for terms")
            .expect_err("missing SPDX identifier must fail");
        assert!(matches!(error, LicenseError::ReadmeDeclaration { .. }));
    }

    #[test]
    fn readme_declaration_rejects_missing_link() {
        let error = require_readme_declaration("licensed under AGPL-3.0-only")
            .expect_err("missing LICENSE link must fail");
        assert!(matches!(error, LicenseError::ReadmeDeclaration { .. }));
    }

    #[test]
    fn license_digest_rejects_mismatch() {
        let error = require_license_digest(b"not the license text")
            .expect_err("wrong license bytes must fail");
        assert!(matches!(error, LicenseError::DigestMismatch { .. }));
    }

    #[test]
    fn missing_license_file_is_an_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let error = read_required(temp.path(), LICENSE_PATH).expect_err("absent LICENSE must fail");
        assert!(matches!(error, LicenseError::Read { .. }));
    }

    #[test]
    fn package_offender_detection() {
        assert_eq!(package_license_offender("good", Some(EXPECTED_SPDX)), None);
        assert_eq!(
            package_license_offender("other", Some("MIT")),
            Some("other (MIT)".to_owned())
        );
        assert_eq!(
            package_license_offender("unlicensed", None),
            Some("unlicensed (none)".to_owned())
        );
    }
}
