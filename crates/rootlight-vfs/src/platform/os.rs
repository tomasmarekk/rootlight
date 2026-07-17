//! Fail-closed mechanics for the Proposed ADR-026 boundary.
//!
//! No target receives a native implementation before the decision is accepted.

use std::io;

use cap_std::fs::Dir;

use super::{PlatformError, PlatformFileIdentity, PrivateName};

#[derive(Debug)]
pub(crate) struct Directory {
    identity: PlatformFileIdentity,
}

#[derive(Debug)]
pub(crate) struct File {
    identity: PlatformFileIdentity,
}

#[derive(Debug)]
pub(crate) struct PublishedDirectory {
    identity: PlatformFileIdentity,
}

pub(crate) fn create_directory(
    _parent: &Dir,
    _name: &PrivateName,
) -> Result<Directory, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

pub(crate) fn create_child(
    _parent: &Directory,
    _name: &PrivateName,
) -> Result<Directory, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

pub(crate) fn create_file(_parent: &Directory, _name: &PrivateName) -> Result<File, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

pub(crate) fn directory_identity(directory: &Directory) -> PlatformFileIdentity {
    directory.identity
}

pub(crate) fn file_identity(file: &File) -> PlatformFileIdentity {
    file.identity
}

pub(crate) fn published_identity(directory: &PublishedDirectory) -> PlatformFileIdentity {
    directory.identity
}

pub(crate) fn sync_directory(_directory: &Directory) -> Result<(), PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

pub(crate) fn sync_published_directory(
    _directory: &PublishedDirectory,
) -> Result<(), PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

pub(crate) fn sync_file(_file: &File) -> Result<(), PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

pub(crate) fn write_file(_file: &mut File, _buffer: &[u8]) -> io::Result<usize> {
    Err(unsupported_io())
}

pub(crate) fn flush_file(_file: &mut File) -> io::Result<()> {
    Err(unsupported_io())
}

pub(crate) fn publish_noreplace(
    directory: Directory,
    _destination_parent: &Dir,
    _destination_name: &PrivateName,
) -> Result<PublishedDirectory, PublishFailure> {
    Err(PublishFailure::NotCommitted {
        directory,
        source: PlatformError::UnsupportedPlatform,
    })
}

pub(crate) fn remove_directory(_directory: Directory) -> Result<(), PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

fn unsupported_io() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "private-tree platform boundary is unsupported",
    )
}

#[derive(Debug)]
pub(crate) enum PublishFailure {
    NotCommitted {
        directory: Directory,
        source: PlatformError,
    },
}
