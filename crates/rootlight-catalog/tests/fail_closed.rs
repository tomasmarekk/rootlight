//! Production-configuration proof for the disabled native boundary.

use rootlight_cancel::Cancellation;
use rootlight_catalog::{
    CATALOG_FILENAME, Catalog, CatalogErrorKind, ORACLE_FILENAME, OracleReader, OracleWriter,
};
use rootlight_storage::{GenerationBudget, GenerationContext};
use tempfile::TempDir;

#[test]
fn path_entry_points_fail_before_filesystem_mutation_or_inspection() {
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
