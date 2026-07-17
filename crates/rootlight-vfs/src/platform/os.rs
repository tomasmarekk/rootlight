//! Platform mechanics for account-private tree handles.
//!
//! This module is private so raw operating-system representations can never
//! escape the safe `platform` API. Native Windows and Apple implementations
//! remain fail-closed until their proposed boundary receives approval.

use std::io;

#[cfg(any(windows, target_vendor = "apple", not(any(unix, windows))))]
use cap_std::fs::Dir;

use super::{PlatformError, PlatformFileIdentity, PrivateName};

#[cfg(all(unix, not(target_vendor = "apple")))]
mod implementation {
    use std::io::{self, Write as _};

    use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
    use cap_std::fs::{
        Dir, DirBuilder, DirBuilderExt as _, File as CapFile, Metadata, MetadataExt as _,
        OpenOptions, OpenOptionsExt as _,
    };
    use rustix::fs::{RenameFlags, renameat_with};

    use super::{PlatformError, PlatformFileIdentity, PrivateName};

    #[derive(Debug)]
    pub(crate) struct Directory {
        parent: Dir,
        name: PrivateName,
        directory: Dir,
        identity: PlatformFileIdentity,
    }

    #[derive(Debug)]
    pub(crate) struct File {
        file: CapFile,
        identity: PlatformFileIdentity,
    }

    #[derive(Debug)]
    pub(crate) struct PublishedDirectory {
        directory: Dir,
        identity: PlatformFileIdentity,
    }

    pub(crate) fn create_directory(
        parent: &Dir,
        name: &PrivateName,
    ) -> Result<Directory, PlatformError> {
        verify_private_directory(parent, "inspect private-tree parent").map_err(
            |error| match error {
                PlatformError::SecurityPolicy => PlatformError::InsecureParent,
                other => other,
            },
        )?;
        create_child_from_parent(parent, name)
    }

    pub(crate) fn create_child(
        parent: &Directory,
        name: &PrivateName,
    ) -> Result<Directory, PlatformError> {
        create_child_from_parent(&parent.directory, name)
    }

    fn create_child_from_parent(
        parent: &Dir,
        name: &PrivateName,
    ) -> Result<Directory, PlatformError> {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        parent
            .create_dir_with(name.as_os_str(), &builder)
            .map_err(|source| PlatformError::io("create directory", source))?;

        let directory = parent
            .open_dir_nofollow(name.as_os_str())
            .map_err(|source| PlatformError::io("open created directory", source))?;
        verify_private_directory(&directory, "verify created directory")?;
        let identity = identity_from_metadata(
            &directory
                .dir_metadata()
                .map_err(|source| PlatformError::io("identify created directory", source))?,
        );
        let parent = parent
            .try_clone()
            .map_err(|source| PlatformError::io("retain private parent", source))?;
        Ok(Directory {
            parent,
            name: name.clone(),
            directory,
            identity,
        })
    }

    pub(crate) fn create_file(
        parent: &Directory,
        name: &PrivateName,
    ) -> Result<File, PlatformError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        options.follow(FollowSymlinks::No);
        let file = parent
            .directory
            .open_with(name.as_os_str(), &options)
            .map_err(|source| PlatformError::io("create private file", source))?;
        let metadata = file
            .metadata()
            .map_err(|source| PlatformError::io("verify private file", source))?;
        verify_private_file(&metadata)?;
        Ok(File {
            identity: identity_from_metadata(&metadata),
            file,
        })
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

    pub(crate) fn sync_directory(directory: &Directory) -> Result<(), PlatformError> {
        sync_dir(&directory.directory, "flush private directory")
    }

    pub(crate) fn sync_published_directory(
        directory: &PublishedDirectory,
    ) -> Result<(), PlatformError> {
        sync_dir(&directory.directory, "flush published directory")
    }

    fn sync_dir(directory: &Dir, operation: &'static str) -> Result<(), PlatformError> {
        let file = directory
            .try_clone()
            .map_err(|source| PlatformError::io(operation, source))?
            .into_std_file();
        file.sync_all()
            .map_err(|source| PlatformError::io(operation, source))
    }

    pub(crate) fn sync_file(file: &File) -> Result<(), PlatformError> {
        file.file
            .sync_all()
            .map_err(|source| PlatformError::io("flush private file", source))
    }

    pub(crate) fn write_file(file: &mut File, buffer: &[u8]) -> io::Result<usize> {
        file.file.write(buffer)
    }

    pub(crate) fn flush_file(file: &mut File) -> io::Result<()> {
        file.file.flush()
    }

    pub(crate) fn publish_noreplace(
        directory: Directory,
        destination_parent: &Dir,
        destination_name: &PrivateName,
    ) -> Result<PublishedDirectory, super::PublishFailure> {
        if let Err(source) = verify_source_entry(&directory) {
            return Err(super::PublishFailure::NotCommitted { directory, source });
        }
        if let Err(source) = sync_dir(&directory.directory, "flush private directory") {
            return Err(super::PublishFailure::NotCommitted { directory, source });
        }
        if let Err(source) = renameat_with(
            &directory.parent,
            directory.name.as_os_str(),
            destination_parent,
            destination_name.as_os_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)
        .map_err(|source| PlatformError::io("publish private directory", source))
        {
            return Err(super::PublishFailure::NotCommitted { directory, source });
        }

        let published = PublishedDirectory {
            directory: directory.directory,
            identity: directory.identity,
        };
        let destination = match destination_parent.try_clone() {
            Ok(directory) => directory.into_std_file(),
            Err(source) => {
                return Err(super::PublishFailure::Committed {
                    directory: published,
                    source,
                });
            }
        };
        if let Err(source) = destination.sync_all() {
            return Err(super::PublishFailure::Committed {
                directory: published,
                source,
            });
        }
        Ok(published)
    }

    pub(crate) fn remove_directory(directory: Directory) -> Result<(), PlatformError> {
        directory
            .directory
            .remove_open_dir_all()
            .map_err(|source| PlatformError::io("remove private directory", source))
    }

    fn verify_source_entry(directory: &Directory) -> Result<(), PlatformError> {
        verify_private_directory(&directory.parent, "reverify private-tree parent").map_err(
            |error| match error {
                PlatformError::SecurityPolicy => PlatformError::InsecureParent,
                other => other,
            },
        )?;
        let metadata = directory
            .parent
            .symlink_metadata(directory.name.as_os_str())
            .map_err(|source| PlatformError::io("identify private source entry", source))?;
        if identity_from_metadata(&metadata) != directory.identity
            || !metadata.is_dir()
            || metadata.file_type().is_symlink()
        {
            return Err(PlatformError::SecurityPolicy);
        }
        Ok(())
    }

    fn verify_private_directory(
        directory: &Dir,
        operation: &'static str,
    ) -> Result<(), PlatformError> {
        let metadata = directory
            .dir_metadata()
            .map_err(|source| PlatformError::io(operation, source))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(PlatformError::SecurityPolicy);
        }
        Ok(())
    }

    fn verify_private_file(metadata: &Metadata) -> Result<(), PlatformError> {
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(PlatformError::SecurityPolicy);
        }
        Ok(())
    }

    fn identity_from_metadata(metadata: &Metadata) -> PlatformFileIdentity {
        PlatformFileIdentity::new(metadata.dev(), u128::from(metadata.ino()))
    }
}

#[cfg(any(windows, target_vendor = "apple", not(any(unix, windows))))]
mod implementation {
    use super::{Dir, PlatformError, PlatformFileIdentity, PrivateName, io};

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

    pub(crate) fn create_file(
        _parent: &Directory,
        _name: &PrivateName,
    ) -> Result<File, PlatformError> {
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
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private-tree platform boundary is unsupported",
        ))
    }

    pub(crate) fn flush_file(_file: &mut File) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private-tree platform boundary is unsupported",
        ))
    }

    pub(crate) fn publish_noreplace(
        directory: Directory,
        _destination_parent: &Dir,
        _destination_name: &PrivateName,
    ) -> Result<PublishedDirectory, super::PublishFailure> {
        Err(super::PublishFailure::NotCommitted {
            directory,
            source: PlatformError::UnsupportedPlatform,
        })
    }

    pub(crate) fn remove_directory(_directory: Directory) -> Result<(), PlatformError> {
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) use implementation::{
    Directory, File, PublishedDirectory, create_child, create_directory, create_file,
    directory_identity, file_identity, flush_file, publish_noreplace, published_identity,
    remove_directory, sync_directory, sync_file, sync_published_directory, write_file,
};

#[derive(Debug)]
pub(crate) enum PublishFailure {
    NotCommitted {
        directory: Directory,
        source: PlatformError,
    },
    #[cfg(all(unix, not(target_vendor = "apple")))]
    Committed {
        directory: PublishedDirectory,
        source: io::Error,
    },
}
