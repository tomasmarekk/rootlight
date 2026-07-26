//! Domain-separated identities for workspace configuration and immutable views.
//!
//! Root and shared-content values are opaque hashes so this layer never needs
//! repository paths, Git object directories, or source text.

use std::fmt;

use rootlight_ids::{ContentHash, content_hash};
use serde::{Deserialize, Serialize};

macro_rules! hash_identity {
    ($name:ident, $summary:literal, $label:literal) => {
        #[doc = $summary]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(ContentHash);

        impl $name {
            /// Creates the identity from a prevalidated immutable content hash.
            #[must_use]
            pub const fn from_hash(hash: ContentHash) -> Self {
                Self(hash)
            }

            /// Returns the underlying immutable content hash.
            #[must_use]
            pub const fn as_hash(self) -> ContentHash {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}:{}", $label, self.0)
            }
        }
    };
}

hash_identity!(
    WorkspaceId,
    "Identity of one independently configured workspace catalog.",
    "workspace"
);
hash_identity!(
    WorkspaceSnapshotId,
    "Identity of an immutable repository-generation membership set.",
    "workspace_snapshot"
);
hash_identity!(
    RepositoryRootIdentity,
    "Opaque canonical identity of one registered repository root.",
    "repository_root"
);
hash_identity!(
    SharedContentIdentity,
    "Opaque identity for immutable Git object data shared by related roots.",
    "shared_content"
);
hash_identity!(
    CrossLinkVersion,
    "Identity of the exact cross-repository linker configuration.",
    "cross_link_version"
);

pub(crate) fn identity_hash(domain: &[u8], fields: &[&[u8]]) -> ContentHash {
    let capacity = domain.len().saturating_add(
        fields
            .iter()
            .map(|field| field.len().saturating_add(8))
            .sum(),
    );
    let mut encoded = Vec::with_capacity(capacity);
    append_field(&mut encoded, domain);
    for field in fields {
        append_field(&mut encoded, field);
    }
    content_hash(&encoded)
}

pub(crate) fn append_field(encoded: &mut Vec<u8>, field: &[u8]) {
    let length = u64::try_from(field.len()).unwrap_or(u64::MAX);
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(field);
}
