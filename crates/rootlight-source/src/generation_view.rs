//! Common source identity access without exposing a partial semantic snapshot.
//! Source-only artifacts and full generations retain distinct capabilities.

use rootlight_ids::FileId;
use rootlight_ir::FileRecord;
use rootlight_storage::{GenerationMetadata, GenerationSnapshot};

use crate::VerifiedSourceAuthority;

pub(super) enum GenerationView<'a> {
    Full(&'a GenerationSnapshot),
    Source(&'a VerifiedSourceAuthority),
}

impl GenerationView<'_> {
    pub(super) fn metadata(&self) -> GenerationMetadata {
        match self {
            Self::Full(value) => value.metadata(),
            Self::Source(value) => value.metadata(),
        }
    }

    pub(super) fn find_file(&self, file: FileId) -> Option<&FileRecord> {
        match self {
            Self::Full(value) => value.find_file(file),
            Self::Source(value) => value.find_file(file),
        }
    }
}
