//! Explicit project-root ownership for native VBA sources.
//! Host formats select the root; this reader never searches unrelated storages
//! for a plausible VBA directory or interprets version-dependent cached code.

use super::{ProjectReadLimits, SourceError, VbaProject};
use crate::compound::{CompoundEntry, CompoundEntryKind, CompoundFile};
use rootlight_cancel::{Cancellation, Cancelled};

/// Failure to bind a VBA project to the root selected by its enclosing format.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RootError {
    /// The selected directory object is absent or is not a storage.
    #[error("vba project root is not a storage")]
    Root,
    /// The root has no unique direct VBA storage child.
    #[error("vba project root has no unique vba storage")]
    Storage,
    /// The root has no unique direct PROJECT stream child.
    #[error("vba project root has no unique properties stream")]
    Properties,
    /// The VBA storage has no unique version stream with its seven-byte prefix.
    #[error("vba project version stream is missing, ambiguous or truncated")]
    Version,
    /// Directory metadata or module binding failed after root ownership resolved.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// A prepared native project with its root-scoped directory evidence.
/// Borrowed descriptors refer to the same immutable compound container as the
/// module reader. This is not host-format validation or full VBA semantics.
#[derive(Debug)]
pub struct RootedProject<'container, 'input> {
    root: &'container CompoundEntry,
    storage: &'container CompoundEntry,
    properties: &'container CompoundEntry,
    version: &'container CompoundEntry,
    project: VbaProject<'container, 'input>,
}

impl<'container, 'input> RootedProject<'container, 'input> {
    /// Opens the project rooted at an explicitly selected compound directory ID.
    ///
    /// A standalone MS-OVBA project part selects the CFB root; an enclosing
    /// legacy host format must establish its own project-root storage identity.
    /// Reserved names ignore ASCII case. All same-name direct children count
    /// toward ambiguity, including objects of the wrong kind. Other subtrees
    /// cannot provide a missing child or override an existing one.
    ///
    /// PROJECT membership and individual bodies remain independently readable
    /// through [`Self::project`]. The version stream's mandatory seven-byte
    /// prefix is admitted by length; all its fields and cached code are ignored
    /// on read, as MS-OVBA 2.3.4.1 requires. No cache bytes are copied or executed.
    ///
    /// # Errors
    /// Returns [`RootError`] for absent/ambiguous/wrong-kind root children, a
    /// truncated version stream, directory parsing limits or cancellation.
    pub fn open(
        compound: &'container CompoundFile<'input>,
        root_id: u32,
        limits: ProjectReadLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, RootError> {
        cancellation.check()?;
        let root = compound
            .entries()
            .find(|entry| entry.id() == root_id)
            .filter(|entry| {
                matches!(
                    entry.kind(),
                    CompoundEntryKind::Root | CompoundEntryKind::Storage
                )
            })
            .ok_or(RootError::Root)?;
        let storage = child(compound, root_id, "VBA", cancellation)?
            .filter(|entry| entry.kind() == CompoundEntryKind::Storage)
            .ok_or(RootError::Storage)?;
        let properties = child(compound, root_id, "PROJECT", cancellation)?
            .filter(|entry| entry.kind() == CompoundEntryKind::Stream)
            .ok_or(RootError::Properties)?;
        let version = child(compound, storage.id(), "_VBA_PROJECT", cancellation)?
            .filter(|entry| entry.kind() == CompoundEntryKind::Stream && entry.byte_length() >= 7)
            .ok_or(RootError::Version)?;
        let project = VbaProject::open(compound, storage.id(), limits, cancellation)?;
        Ok(Self {
            root,
            storage,
            properties,
            version,
            project,
        })
    }

    /// Returns the selected root's original CFB identity and descriptor range.
    #[must_use]
    pub const fn root(&self) -> &CompoundEntry {
        self.root
    }

    /// Returns the root's unique direct VBA storage descriptor.
    #[must_use]
    pub const fn storage(&self) -> &CompoundEntry {
        self.storage
    }

    /// Returns the root's unique direct PROJECT stream descriptor.
    #[must_use]
    pub const fn properties(&self) -> &CompoundEntry {
        self.properties
    }

    /// Returns the required version-stream descriptor without interpreting its cache.
    #[must_use]
    pub const fn version(&self) -> &CompoundEntry {
        self.version
    }

    /// Returns the lazy reader confined to this root's VBA storage.
    #[must_use]
    pub const fn project(&self) -> &VbaProject<'container, 'input> {
        &self.project
    }
}

fn child<'container>(
    compound: &'container CompoundFile<'_>,
    parent: u32,
    name: &str,
    cancellation: &Cancellation,
) -> Result<Option<&'container CompoundEntry>, Cancelled> {
    let mut selected = None;
    for entry in compound.entries() {
        cancellation.check()?;
        if entry.parent() == Some(parent) && entry.name().eq_ignore_ascii_case(name) {
            if selected.is_some() {
                return Ok(None);
            }
            selected = Some(entry);
        }
    }
    Ok(selected)
}
