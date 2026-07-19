//! Production-configuration proof for the disabled native boundary.

use std::fs;

use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationIdentity, content_hash, derive_generation, derive_repository};
use rootlight_search::{ArtifactBudget, BuildBudget, LexicalIndexBuilder, SearchError};
use tempfile::TempDir;

#[test]
fn path_builder_fails_before_filesystem_mutation_or_inspection() {
    let directory = TempDir::new().expect("temporary parent exists");
    let sentinel = directory.path().join("foreign");
    fs::write(&sentinel, b"untouched").expect("sentinel writes");
    let missing = directory.path().join("missing");
    let cancellation = Cancellation::new();

    for path in [directory.path(), missing.as_path()] {
        assert_eq!(
            LexicalIndexBuilder::build(
                path,
                generation(),
                Vec::new(),
                BuildBudget::default(),
                ArtifactBudget::default(),
                &cancellation,
            ),
            Err(SearchError::UnsupportedPrivateFileBoundary)
        );
    }

    assert_eq!(
        fs::read(&sentinel).expect("sentinel remains readable"),
        b"untouched"
    );
    assert!(!missing.exists());
}

fn generation() -> rootlight_ids::GenerationId {
    let repository = derive_repository(b"fail-closed-search-fixture").id();
    derive_generation(GenerationIdentity {
        repository,
        parent: None,
        manifest_hash: content_hash(b"manifest"),
        config_hash: content_hash(b"config"),
        provider_set_hash: content_hash(b"providers"),
        format_version: 1,
    })
    .id()
}
