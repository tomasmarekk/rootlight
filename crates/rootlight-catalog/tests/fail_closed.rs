//! Production-configuration proof for the native private-file boundary.

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use rootlight_cancel::Cancellation;
use rootlight_catalog::{CATALOG_FILENAME, Catalog, ORACLE_FILENAME, OracleWriter};
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use rootlight_catalog::{CatalogErrorKind, OracleReader};
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use rootlight_storage::{GenerationBudget, GenerationContext};
use tempfile::TempDir;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn supported_path_entry_points_create_private_databases() {
    let control_directory = TempDir::new().expect("temporary control parent exists");
    let generation_directory = TempDir::new().expect("temporary generation parent exists");

    let catalog = Catalog::open_in(control_directory.path()).expect("control catalog initializes");
    catalog.verify().expect("control catalog verifies");
    drop(
        OracleWriter::create_in(generation_directory.path())
            .expect("oracle database initializes exclusively"),
    );

    assert!(control_directory.path().join(CATALOG_FILENAME).is_file());
    assert!(generation_directory.path().join(ORACLE_FILENAME).is_file());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn supported_unix_control_catalog_rejects_a_final_symlink() {
    use std::os::unix::fs::symlink;

    let target_directory = TempDir::new().expect("temporary target parent exists");
    let target_path = target_directory.path().join(CATALOG_FILENAME);
    drop(Catalog::open_in(target_directory.path()).expect("target catalog initializes"));

    let link_directory = TempDir::new().expect("temporary link parent exists");
    symlink(&target_path, link_directory.path().join(CATALOG_FILENAME))
        .expect("catalog symlink is created");

    let error = Catalog::open_in(link_directory.path())
        .expect_err("a final symlink must fail before SQLite opens it");
    assert_eq!(
        error.kind(),
        rootlight_catalog::CatalogErrorKind::InsecureFile
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_control_catalog_rejects_an_extended_acl() {
    let directory = TempDir::new().expect("temporary catalog parent exists");
    let catalog_path = directory.path().join(CATALOG_FILENAME);
    drop(Catalog::open_in(directory.path()).expect("catalog initializes"));

    let status = std::process::Command::new("/bin/chmod")
        .arg("+a")
        .arg("everyone allow read")
        .arg(&catalog_path)
        .status()
        .expect("macOS chmod executes");
    assert!(status.success(), "macOS test ACL is installed");

    let error = Catalog::open_in(directory.path())
        .expect_err("an extended ACL must fail before SQLite opens");
    assert_eq!(
        error.kind(),
        rootlight_catalog::CatalogErrorKind::InsecureFile
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
#[test]
fn unsupported_path_entry_points_fail_before_filesystem_mutation_or_inspection() {
    let directory = TempDir::new().expect("temporary parent exists");
    let expected = CatalogErrorKind::UnsupportedPrivateFileBoundary;

    assert_eq!(
        Catalog::open_in(directory.path())
            .expect_err("control path entry point fails closed")
            .kind(),
        expected
    );
    assert_eq!(
        OracleWriter::create_in(directory.path())
            .expect_err("oracle write path entry point fails closed")
            .kind(),
        expected
    );
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    assert_eq!(
        OracleReader::open_in(directory.path(), &context)
            .expect_err("oracle read path entry point fails closed")
            .kind(),
        expected
    );
    assert!(!directory.path().join(CATALOG_FILENAME).exists());
    assert!(!directory.path().join(ORACLE_FILENAME).exists());
}
