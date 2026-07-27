//! Handle-relative mechanics for account-private trees.
//!
//! Linux and macOS use their native non-replacing rename through safe `rustix`
//! wrappers. Unix objects are verified by retained descriptor, owner, mode,
//! link count, and, on macOS, the absence of an inherited extended ACL.
//! Windows inherits a verified protected parent DACL, immediately protects the
//! retained child handle, and relies on the operating system's non-replacing
//! directory rename.

#![allow(
    unsafe_code,
    reason = "macOS descriptor ACL APIs have no safe standard-library or rustix wrapper"
)]

use std::{
    io::{self, Read as _, Write as _},
    path::Path,
};

use cap_std::fs::Dir;

use super::{PlatformError, PlatformFileIdentity, PrivateName};

#[cfg(target_os = "macos")]
use std::ffi::c_void;

#[cfg(any(unix, windows))]
use cap_fs_ext::{
    DirExt as _, FollowSymlinks, MetadataExt as CrossPlatformMetadataExt, OpenOptionsFollowExt as _,
};
#[cfg(any(unix, windows))]
use cap_std::fs::{File as CapFile, Metadata, OpenOptions};
#[cfg(any(unix, windows))]
use std::ffi::OsString;

#[derive(Debug)]
pub(crate) struct Directory {
    #[cfg(any(unix, windows))]
    dir: Option<Dir>,
    #[cfg(any(unix, windows))]
    parent: Dir,
    #[cfg(any(unix, windows))]
    name: OsString,
    #[cfg(windows)]
    identity_handle: CapFile,
    identity: PlatformFileIdentity,
}

#[derive(Debug)]
pub(crate) struct File {
    #[cfg(any(unix, windows))]
    file: CapFile,
    identity: PlatformFileIdentity,
}

#[derive(Debug)]
pub(crate) struct PublishedDirectory {
    #[cfg(any(unix, windows))]
    dir: Option<Dir>,
    #[cfg(any(unix, windows))]
    parent: Dir,
    #[cfg(windows)]
    identity_handle: CapFile,
    identity: PlatformFileIdentity,
}

pub(crate) fn require_support() -> Result<(), PlatformError> {
    #[cfg(any(unix, windows))]
    {
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn verify_parent(parent: &Dir) -> Result<(), PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        validate_private_directory(parent)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn create_directory(
    parent: &Dir,
    name: &PrivateName,
) -> Result<Directory, PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        validate_private_directory(parent)?;
        create_private_directory(parent, name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name);
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn open_directory(parent: &Dir, name: &PrivateName) -> Result<Directory, PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        validate_private_directory(parent)?;
        open_private_directory(parent, name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name);
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn create_child(
    parent: &Directory,
    name: &PrivateName,
) -> Result<Directory, PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        let parent = directory_handle(parent)?;
        validate_private_directory(parent)?;
        create_private_directory(parent, name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name);
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn create_file(parent: &Directory, name: &PrivateName) -> Result<File, PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        let parent = directory_handle(parent)?;
        validate_private_directory(parent)?;
        create_private_file(parent, name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name);
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn secure_parent(parent: &mut Dir) -> Result<(), PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        harden_private_directory(parent)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn create_standalone_file(
    parent: &Dir,
    name: &PrivateName,
) -> Result<File, PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        validate_private_directory(parent)?;
        create_private_file(parent, name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name);
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn sync_parent(parent: &Dir) -> Result<(), PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        validate_private_directory(parent)?;
        sync_dir_handle(parent)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn read_file_bounded(
    parent: &Directory,
    name: &PrivateName,
    maximum_bytes: u64,
) -> Result<Vec<u8>, PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        let parent = directory_handle(parent)?;
        validate_private_directory(parent)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            use windows::Win32::Storage::FileSystem::{
                FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ,
            };

            options
                .access_mode(FILE_GENERIC_READ.0)
                .share_mode((FILE_SHARE_READ | FILE_SHARE_DELETE).0);
        }
        let mut file = parent
            .open_with(Path::new(&name.0), &options)
            .map_err(|source| platform_io("open_file", source))?;
        #[cfg(windows)]
        verify_private_windows_dacl(&file)?;
        #[cfg(target_os = "macos")]
        verify_no_macos_extended_acl(&file)?;
        let before = file
            .metadata()
            .map_err(|source| platform_io("inspect_file", source))?;
        let identity = validate_private_file_metadata(&before)?;
        if before.len() > maximum_bytes {
            return Err(PlatformError::ResourceLimit);
        }
        let reserve = usize::try_from(before.len()).map_err(|_| PlatformError::ResourceLimit)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(reserve)
            .map_err(|_| PlatformError::ResourceLimit)?;
        (&mut file)
            .take(maximum_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| platform_io("read_file", source))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
            return Err(PlatformError::ResourceLimit);
        }
        let after = file
            .metadata()
            .map_err(|source| platform_io("inspect_file", source))?;
        if validate_private_file_metadata(&after)? != identity
            || before.len() != after.len()
            || after.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
        {
            return Err(PlatformError::SecurityPolicy);
        }
        Ok(bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, maximum_bytes);
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn directory_identity(directory: &Directory) -> PlatformFileIdentity {
    directory.identity
}

pub(crate) fn directory_capability(directory: &Directory) -> &Dir {
    directory_handle(directory).expect("validated private directory retains its capability")
}

pub(crate) fn file_identity(file: &File) -> PlatformFileIdentity {
    file.identity
}

pub(crate) fn published_identity(directory: &PublishedDirectory) -> PlatformFileIdentity {
    directory.identity
}

pub(crate) fn sync_directory(directory: &Directory) -> Result<(), PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        sync_dir_handle(directory_handle(directory)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn sync_published_directory(
    directory: &PublishedDirectory,
) -> Result<(), PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        let directory = directory
            .dir
            .as_ref()
            .ok_or(PlatformError::SecurityPolicy)?;
        sync_dir_handle(directory)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn sync_file(file: &File) -> Result<(), PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        let metadata = file
            .file
            .metadata()
            .map_err(|source| platform_io("inspect_file_before_sync", source))?;
        if validate_private_file_metadata(&metadata)? != file.identity {
            return Err(PlatformError::SecurityPolicy);
        }
        #[cfg(target_os = "macos")]
        verify_no_macos_extended_acl(&file.file)?;
        #[cfg(windows)]
        verify_private_windows_dacl(&file.file)?;
        file.file
            .sync_all()
            .map_err(|source| platform_io("sync_file", source))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn write_file(file: &mut File, buffer: &[u8]) -> io::Result<usize> {
    #[cfg(any(unix, windows))]
    {
        file.file.write(buffer)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, buffer);
        Err(unsupported_io())
    }
}

pub(crate) fn flush_file(file: &mut File) -> io::Result<()> {
    #[cfg(any(unix, windows))]
    {
        file.file.flush()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(unsupported_io())
    }
}

pub(crate) fn publish_noreplace(
    directory: Directory,
    destination_parent: &Dir,
    destination_name: &PrivateName,
) -> Result<PublishedDirectory, PublishFailure> {
    if let Err(source) = require_support() {
        return Err(PublishFailure::NotCommitted {
            directory: Box::new(directory),
            source,
        });
    }
    #[cfg(unix)]
    {
        publish_noreplace_unix(directory, destination_parent, destination_name)
    }
    #[cfg(windows)]
    {
        publish_noreplace_windows(directory, destination_parent, destination_name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (destination_parent, destination_name);
        Err(PublishFailure::NotCommitted {
            directory: Box::new(directory),
            source: PlatformError::UnsupportedPlatform,
        })
    }
}

pub(crate) fn remove_directory(directory: Directory) -> Result<(), PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        let Directory {
            dir,
            parent,
            name: _,
            #[cfg(windows)]
            identity_handle,
            identity: _,
        } = directory;
        #[cfg(windows)]
        drop(identity_handle);
        let dir = dir.ok_or(PlatformError::SecurityPolicy)?;
        dir.remove_open_dir_all()
            .map_err(|source| platform_io("remove_directory", source))?;
        sync_dir_handle(&parent)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        Err(PlatformError::UnsupportedPlatform)
    }
}

pub(crate) fn remove_published_directory(
    directory: PublishedDirectory,
) -> Result<(), PlatformError> {
    require_support()?;
    #[cfg(any(unix, windows))]
    {
        let PublishedDirectory {
            dir,
            parent,
            #[cfg(windows)]
            identity_handle,
            identity: _,
        } = directory;
        #[cfg(windows)]
        drop(identity_handle);
        let dir = dir.ok_or(PlatformError::SecurityPolicy)?;
        dir.remove_open_dir_all()
            .map_err(|source| platform_io("remove_published_directory", source))?;
        sync_dir_handle(&parent)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        Err(PlatformError::UnsupportedPlatform)
    }
}

#[cfg(any(unix, windows))]
fn create_private_directory(parent: &Dir, name: &PrivateName) -> Result<Directory, PlatformError> {
    parent
        .create_dir(Path::new(&name.0))
        .map_err(|source| platform_io("create_directory", source))?;
    let opened = parent
        .open_dir_nofollow(Path::new(&name.0))
        .map_err(|source| platform_io("open_directory", source));
    let mut dir = match opened {
        Ok(dir) => dir,
        Err(error) => {
            let _ = parent.remove_dir(Path::new(&name.0));
            return Err(error);
        }
    };
    #[cfg(windows)]
    let identity_handle = match harden_named_private_directory(parent, name) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = dir.remove_open_dir_all();
            return Err(error);
        }
    };
    if let Err(error) = harden_private_directory(&mut dir) {
        let _ = dir.remove_open_dir_all();
        return Err(error);
    }
    let metadata = dir
        .dir_metadata()
        .map_err(|source| platform_io("inspect_directory", source))?;
    let identity = validate_private_directory_metadata(&metadata)?;
    #[cfg(windows)]
    {
        let retained_metadata = identity_handle
            .metadata()
            .map_err(|source| platform_io("inspect_directory_identity", source))?;
        if validate_private_directory_metadata(&retained_metadata)? != identity {
            let _ = dir.remove_open_dir_all();
            return Err(PlatformError::SecurityPolicy);
        }
    }
    if let Err(error) = sync_dir_handle(parent) {
        let _ = dir.remove_open_dir_all();
        let _ = sync_dir_handle(parent);
        return Err(error);
    }
    Ok(Directory {
        dir: Some(dir),
        parent: parent
            .try_clone()
            .map_err(|source| platform_io("clone_parent", source))?,
        name: name.0.clone(),
        #[cfg(windows)]
        identity_handle,
        identity,
    })
}

#[cfg(any(unix, windows))]
fn create_private_file(parent: &Dir, name: &PrivateName) -> Result<File, PlatformError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        use windows::Win32::Storage::FileSystem::{
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, WRITE_DAC,
        };

        options
            .access_mode((FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC).0)
            .share_mode(0);
    }
    let mut file = parent
        .open_with(Path::new(&name.0), &options)
        .map_err(|source| platform_io("create_file", source))?;
    if let Err(error) = harden_private_file(&mut file) {
        drop(file);
        let _ = parent.remove_file(Path::new(&name.0));
        return Err(error);
    }
    let metadata = file
        .metadata()
        .map_err(|source| platform_io("inspect_file", source))?;
    let identity = validate_private_file_metadata(&metadata)?;
    Ok(File { file, identity })
}

#[cfg(any(unix, windows))]
fn open_private_directory(parent: &Dir, name: &PrivateName) -> Result<Directory, PlatformError> {
    let dir = parent
        .open_dir_nofollow(Path::new(&name.0))
        .map_err(|source| platform_io("open_directory", source))?;
    validate_private_directory(&dir)?;
    let metadata = dir
        .dir_metadata()
        .map_err(|source| platform_io("inspect_directory", source))?;
    let identity = validate_private_directory_metadata(&metadata)?;
    #[cfg(windows)]
    let identity_handle = open_named_private_directory_identity(parent, name, false)?;
    #[cfg(windows)]
    {
        let retained_metadata = identity_handle
            .metadata()
            .map_err(|source| platform_io("inspect_directory_identity", source))?;
        if validate_private_directory_metadata(&retained_metadata)? != identity {
            return Err(PlatformError::SecurityPolicy);
        }
    }
    Ok(Directory {
        dir: Some(dir),
        parent: parent
            .try_clone()
            .map_err(|source| platform_io("clone_parent", source))?,
        name: name.0.clone(),
        #[cfg(windows)]
        identity_handle,
        identity,
    })
}

#[cfg(any(unix, windows))]
fn directory_handle(directory: &Directory) -> Result<&Dir, PlatformError> {
    directory.dir.as_ref().ok_or(PlatformError::SecurityPolicy)
}

#[cfg(any(unix, windows))]
fn prepare_publication(
    directory: &Directory,
    destination_parent: &Dir,
) -> Result<(), PlatformError> {
    validate_private_directory(&directory.parent)?;
    validate_private_directory(directory_handle(directory)?)?;
    validate_private_directory(destination_parent)?;
    verify_named_directory_identity(&directory.parent, &directory.name, directory.identity)?;
    sync_dir_handle(directory_handle(directory)?)
}

#[cfg(any(unix, windows))]
fn verify_named_directory_identity(
    parent: &Dir,
    name: &std::ffi::OsStr,
    expected: PlatformFileIdentity,
) -> Result<(), PlatformError> {
    let observed = parent
        .open_dir_nofollow(Path::new(name))
        .map_err(|source| platform_io("reopen_directory", source))?;
    validate_private_directory(&observed)?;
    let metadata = observed
        .dir_metadata()
        .map_err(|source| platform_io("inspect_directory", source))?;
    let actual = validate_private_directory_metadata(&metadata)?;
    if actual != expected {
        return Err(PlatformError::SecurityPolicy);
    }
    Ok(())
}

#[cfg(unix)]
fn publish_noreplace_unix(
    directory: Directory,
    destination_parent: &Dir,
    destination_name: &PrivateName,
) -> Result<PublishedDirectory, PublishFailure> {
    if let Err(source) = prepare_publication(&directory, destination_parent) {
        return Err(PublishFailure::NotCommitted {
            directory: Box::new(directory),
            source,
        });
    }
    let parent = match destination_parent.try_clone() {
        Ok(parent) => parent,
        Err(source) => {
            return Err(PublishFailure::NotCommitted {
                directory: Box::new(directory),
                source: platform_io("clone_destination_parent", source),
            });
        }
    };
    if let Err(source) = rename_noreplace_unix(&directory, destination_parent, destination_name) {
        return Err(PublishFailure::NotCommitted {
            directory: Box::new(directory),
            source: platform_io("publish_directory", source),
        });
    }

    let published = PublishedDirectory {
        dir: directory.dir,
        parent,
        identity: directory.identity,
    };
    finish_publication(published, destination_parent, destination_name)
}

#[cfg(windows)]
fn publish_noreplace_windows(
    mut directory: Directory,
    destination_parent: &Dir,
    destination_name: &PrivateName,
) -> Result<PublishedDirectory, PublishFailure> {
    if let Err(source) = prepare_publication(&directory, destination_parent) {
        return Err(PublishFailure::NotCommitted {
            directory: Box::new(directory),
            source,
        });
    }
    let parent = match destination_parent.try_clone() {
        Ok(parent) => parent,
        Err(source) => {
            return Err(PublishFailure::NotCommitted {
                directory: Box::new(directory),
                source: platform_io("clone_destination_parent", source),
            });
        }
    };
    drop(directory.dir.take());
    let destination = Path::new(&destination_name.0);
    match destination_parent.symlink_metadata(destination) {
        Ok(_) => {
            restore_source_directory_handle(&mut directory);
            return Err(PublishFailure::NotCommitted {
                directory: Box::new(directory),
                source: platform_io(
                    "publish_directory",
                    io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "private-tree destination already exists",
                    ),
                ),
            });
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            restore_source_directory_handle(&mut directory);
            return Err(PublishFailure::NotCommitted {
                directory: Box::new(directory),
                source: platform_io("inspect_destination", source),
            });
        }
    }
    if let Err(source) =
        directory
            .parent
            .rename(Path::new(&directory.name), destination_parent, destination)
    {
        restore_source_directory_handle(&mut directory);
        return Err(PublishFailure::NotCommitted {
            directory: Box::new(directory),
            source: platform_io("publish_directory", source),
        });
    }

    let destination_handle = destination_parent.open_dir_nofollow(destination).ok();
    let published = PublishedDirectory {
        dir: destination_handle,
        parent,
        identity_handle: directory.identity_handle,
        identity: directory.identity,
    };
    finish_publication(published, destination_parent, destination_name)
}

#[cfg(windows)]
fn restore_source_directory_handle(directory: &mut Directory) {
    directory.dir = directory
        .parent
        .open_dir_nofollow(Path::new(&directory.name))
        .ok();
}

#[cfg(any(unix, windows))]
fn finish_publication(
    published: PublishedDirectory,
    destination_parent: &Dir,
    destination_name: &PrivateName,
) -> Result<PublishedDirectory, PublishFailure> {
    if let Err(source) = verify_published_identity(&published, destination_parent, destination_name)
    {
        return Err(PublishFailure::CommittedButDurabilityUnknown {
            directory: Box::new(published),
            source,
        });
    }
    if let Err(source) = sync_dir_handle(destination_parent) {
        return Err(PublishFailure::CommittedButDurabilityUnknown {
            directory: Box::new(published),
            source: platform_error_to_io(source),
        });
    }
    Ok(published)
}

#[cfg(any(unix, windows))]
fn verify_published_identity(
    directory: &PublishedDirectory,
    destination_parent: &Dir,
    destination_name: &PrivateName,
) -> io::Result<()> {
    let observed = destination_parent.open_dir_nofollow(Path::new(&destination_name.0))?;
    validate_private_directory(&observed).map_err(platform_error_to_io)?;
    let metadata = observed.dir_metadata()?;
    let actual = validate_private_directory_metadata(&metadata).map_err(platform_error_to_io)?;
    if actual != directory.identity {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "published private-tree identity changed",
        ));
    }
    #[cfg(windows)]
    verify_private_windows_dacl(&directory.identity_handle).map_err(platform_error_to_io)?;
    Ok(())
}

#[cfg(unix)]
fn rename_noreplace_unix(
    directory: &Directory,
    destination_parent: &Dir,
    destination_name: &PrivateName,
) -> io::Result<()> {
    use rustix::fs::{RenameFlags, renameat_with};

    renameat_with(
        &directory.parent,
        Path::new(&directory.name),
        destination_parent,
        Path::new(&destination_name.0),
        RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)
}

#[cfg(any(unix, windows))]
fn validate_private_directory(directory: &Dir) -> Result<(), PlatformError> {
    let metadata = directory
        .dir_metadata()
        .map_err(|source| platform_io("inspect_directory", source))?;
    validate_private_directory_metadata(&metadata)?;
    #[cfg(target_os = "macos")]
    verify_no_macos_extended_acl(directory)?;
    #[cfg(windows)]
    verify_private_windows_dacl(directory)?;
    Ok(())
}

#[cfg(any(unix, windows))]
fn validate_private_directory_metadata(
    metadata: &Metadata,
) -> Result<PlatformFileIdentity, PlatformError> {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(PlatformError::InsecureParent);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt as _;

        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(PlatformError::InsecureParent);
        }
    }
    Ok(metadata_identity(metadata))
}

#[cfg(any(unix, windows))]
fn validate_private_file_metadata(
    metadata: &Metadata,
) -> Result<PlatformFileIdentity, PlatformError> {
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || CrossPlatformMetadataExt::nlink(metadata) != 1
    {
        return Err(PlatformError::SecurityPolicy);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt as _;

        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(PlatformError::SecurityPolicy);
        }
    }
    Ok(metadata_identity(metadata))
}

#[cfg(any(unix, windows))]
fn metadata_identity(metadata: &Metadata) -> PlatformFileIdentity {
    PlatformFileIdentity {
        volume: CrossPlatformMetadataExt::dev(metadata),
        file: u128::from(CrossPlatformMetadataExt::ino(metadata)),
    }
}

#[cfg(unix)]
fn harden_private_directory(directory: &mut Dir) -> Result<(), PlatformError> {
    use std::os::unix::fs::PermissionsExt as _;

    // cap-std may retain an O_PATH descriptor on Linux. Updating "." keeps the
    // mutation handle-relative without requiring fchmod on that descriptor.
    directory
        .set_permissions(
            Path::new("."),
            cap_std::fs::Permissions::from_std(std::fs::Permissions::from_mode(0o700)),
        )
        .map_err(|source| platform_io("protect_directory", source))?;
    #[cfg(target_os = "macos")]
    clear_macos_extended_acl(directory)?;
    validate_private_directory(directory)
}

#[cfg(windows)]
fn harden_private_directory(directory: &mut Dir) -> Result<(), PlatformError> {
    validate_private_directory(directory)
}

#[cfg(windows)]
fn harden_named_private_directory(
    parent: &Dir,
    name: &PrivateName,
) -> Result<CapFile, PlatformError> {
    open_named_private_directory_identity(parent, name, true)
}

#[cfg(windows)]
fn open_named_private_directory_identity(
    parent: &Dir,
    name: &PrivateName,
    protect: bool,
) -> Result<CapFile, PlatformError> {
    use cap_fs_ext::OpenOptionsMaybeDirExt as _;
    use cap_std::fs::OpenOptionsExt as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, WRITE_DAC,
    };

    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .maybe_dir(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0);
    options.access_mode(if protect {
        (FILE_GENERIC_READ | WRITE_DAC).0
    } else {
        FILE_GENERIC_READ.0
    });
    let mut handle = parent
        .open_with(Path::new(&name.0), &options)
        .map_err(|source| platform_io("open_directory_security", source))?;
    if protect {
        apply_private_windows_dacl(&mut handle)?;
    }
    verify_private_windows_dacl(&handle)?;

    let expected = validate_private_directory_metadata(
        &handle
            .metadata()
            .map_err(|source| platform_io("inspect_directory_identity", source))?,
    )?;
    let protected_handle = handle.into_std();
    let path = winx::file::get_file_path(&protected_handle)
        .map_err(|source| platform_io("resolve_directory_identity", source))?;
    let retained = std::fs::OpenOptions::new()
        .access_mode(FILE_GENERIC_READ.0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
        .open(path)
        .map_err(|source| platform_io("reopen_directory_identity", source))?;
    let retained = CapFile::from_std(retained);
    verify_private_windows_dacl(&retained)?;
    let actual = validate_private_directory_metadata(
        &retained
            .metadata()
            .map_err(|source| platform_io("inspect_directory_identity", source))?,
    )?;
    if actual != expected {
        return Err(PlatformError::SecurityPolicy);
    }
    drop(protected_handle);
    Ok(retained)
}

#[cfg(unix)]
fn harden_private_file(file: &mut CapFile) -> Result<(), PlatformError> {
    use cap_std::fs::PermissionsExt as _;

    file.set_permissions(cap_std::fs::Permissions::from_mode(0o600))
        .map_err(|source| platform_io("protect_file", source))?;
    #[cfg(target_os = "macos")]
    clear_macos_extended_acl(file)?;
    let metadata = file
        .metadata()
        .map_err(|source| platform_io("inspect_file", source))?;
    validate_private_file_metadata(&metadata)?;
    #[cfg(target_os = "macos")]
    verify_no_macos_extended_acl(file)?;
    Ok(())
}

#[cfg(windows)]
fn harden_private_file(file: &mut CapFile) -> Result<(), PlatformError> {
    apply_private_windows_dacl(file)?;
    verify_private_windows_dacl(file)?;
    let metadata = file
        .metadata()
        .map_err(|source| platform_io("inspect_file", source))?;
    validate_private_file_metadata(&metadata).map(|_| ())
}

#[cfg(target_os = "linux")]
fn sync_dir_handle(directory: &Dir) -> Result<(), PlatformError> {
    use rustix::fs::{Mode, OFlags};

    // cap-std may retain an O_PATH descriptor on Linux. Reopening "." through
    // that capability yields a directory descriptor that fsync can flush.
    let sync_handle = rustix::fs::openat(
        directory,
        Path::new("."),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|source| platform_io("open_directory_for_sync", io::Error::from(source)))?;
    rustix::fs::fsync(&sync_handle)
        .map_err(|source| platform_io("sync_directory", io::Error::from(source)))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn sync_dir_handle(directory: &Dir) -> Result<(), PlatformError> {
    directory
        .try_clone()
        .map(Dir::into_std_file)
        .and_then(|file| file.sync_all())
        .map_err(|source| platform_io("sync_directory", source))
}

#[cfg(target_os = "macos")]
const MACOS_ACL_TYPE_EXTENDED: u32 = 256;
#[cfg(target_os = "macos")]
const MACOS_FILESEC_ACL: std::ffi::c_int = 5;
#[cfg(target_os = "macos")]
const MACOS_FILESEC_REMOVE_ACL_ADDRESS: usize = 1;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_free(object: *mut c_void) -> std::ffi::c_int;
    fn acl_get_fd_np(fd: std::ffi::c_int, acl_type: u32) -> *mut c_void;
    fn fchmodx_np(fd: std::ffi::c_int, file_security: *mut c_void) -> std::ffi::c_int;
    fn filesec_free(file_security: *mut c_void);
    fn filesec_init() -> *mut c_void;
    fn filesec_set_property(
        file_security: *mut c_void,
        property: std::ffi::c_int,
        value: *const c_void,
    ) -> std::ffi::c_int;
}

#[cfg(target_os = "macos")]
fn clear_macos_extended_acl<H: std::os::fd::AsRawFd>(handle: &H) -> Result<(), PlatformError> {
    // SAFETY: `filesec_init` creates a process-owned opaque allocation. A
    // non-null result is released exactly once below with `filesec_free`.
    let file_security = unsafe { filesec_init() };
    if file_security.is_null() {
        return Err(platform_io(
            "create_file_security",
            io::Error::last_os_error(),
        ));
    }

    let remove_acl = std::ptr::without_provenance::<c_void>(MACOS_FILESEC_REMOVE_ACL_ADDRESS);
    // SAFETY: `file_security` is the live allocation returned above. Darwin
    // defines pointer value 1 as the non-dereferenced ACL-removal sentinel for
    // `FILESEC_ACL`; the descriptor is retained by `handle` for `fchmodx_np`.
    let removal_error = unsafe {
        let set_result = filesec_set_property(file_security, MACOS_FILESEC_ACL, remove_acl);
        let error = if set_result != 0 {
            Some(io::Error::last_os_error())
        } else if fchmodx_np(handle.as_raw_fd(), file_security) != 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        filesec_free(file_security);
        error
    };
    if let Some(source) = removal_error {
        return Err(platform_io("remove_extended_acl", source));
    }
    verify_no_macos_extended_acl(handle)
}

#[cfg(target_os = "macos")]
fn verify_no_macos_extended_acl<H: std::os::fd::AsRawFd>(handle: &H) -> Result<(), PlatformError> {
    // SAFETY: the descriptor remains valid for the call and Darwin returns
    // either a process-owned ACL allocation or null with errno.
    let acl = unsafe { acl_get_fd_np(handle.as_raw_fd(), MACOS_ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let source = io::Error::last_os_error();
        return if source.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(platform_io("inspect_extended_acl", source))
        };
    }

    // SAFETY: `acl` is the non-null allocation returned immediately above and
    // this is its single release.
    let free_result = unsafe { acl_free(acl) };
    if free_result != 0 {
        return Err(platform_io(
            "release_extended_acl",
            io::Error::last_os_error(),
        ));
    }
    Err(PlatformError::SecurityPolicy)
}

#[cfg(windows)]
fn sync_dir_handle(directory: &Dir) -> Result<(), PlatformError> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let expected = validate_private_directory_metadata(
        &directory
            .dir_metadata()
            .map_err(|source| platform_io("inspect_directory_before_sync", source))?,
    )?;
    let identity_handle = directory
        .try_clone()
        .map(Dir::into_std_file)
        .map_err(|source| platform_io("clone_directory_for_sync", source))?;
    let path = winx::file::get_file_path(&identity_handle)
        .map_err(|source| platform_io("resolve_directory_for_sync", source))?;
    let flush_handle = std::fs::OpenOptions::new()
        .access_mode((FILE_GENERIC_READ | FILE_GENERIC_WRITE).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
        .open(path)
        .map(CapFile::from_std)
        .map_err(|source| platform_io("open_directory_for_sync", source))?;
    verify_private_windows_dacl(&flush_handle)?;
    let actual = validate_private_directory_metadata(
        &flush_handle
            .metadata()
            .map_err(|source| platform_io("inspect_directory_sync_identity", source))?,
    )?;
    if actual != expected {
        return Err(PlatformError::SecurityPolicy);
    }
    flush_handle
        .sync_all()
        .map_err(|source| platform_io("sync_directory", source))
}

#[cfg(windows)]
fn apply_private_windows_dacl<H: std::os::windows::io::AsRawHandle>(
    handle: &mut H,
) -> Result<(), PlatformError> {
    use windows_permissions::{
        constants::{SeObjectType, SecurityInformation},
        wrappers::SetSecurityInfo,
    };

    let descriptor = private_windows_descriptor()?;
    let dacl = descriptor.dacl().ok_or(PlatformError::SecurityPolicy)?;
    SetSecurityInfo(
        handle,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        Some(dacl),
        None,
    )
    .map_err(|source| platform_io("protect_windows_object", source))
}

#[cfg(windows)]
fn verify_private_windows_dacl<H: std::os::windows::io::AsRawHandle>(
    handle: &H,
) -> Result<(), PlatformError> {
    use windows_permissions::{
        constants::{SeObjectType, SecurityInformation},
        wrappers::GetSecurityInfo,
    };

    let expected_sid = current_windows_user_sid()?;
    let descriptor = GetSecurityInfo(
        handle,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
    )
    .map_err(|source| platform_io("inspect_windows_dacl", source))?;
    let dacl = descriptor.dacl().ok_or(PlatformError::SecurityPolicy)?;
    if dacl.len() != 1 {
        return Err(PlatformError::SecurityPolicy);
    }
    let ace = dacl.get_ace(0).ok_or(PlatformError::SecurityPolicy)?;
    if ace.ace_type() != windows_permissions::constants::AceType::ACCESS_ALLOWED_ACE_TYPE
        || ace.mask() != windows_permissions::constants::AccessRights::FileAllAccess
        || !ace.flags().is_empty()
        || ace.sid().ok_or(PlatformError::SecurityPolicy)?.to_string() != expected_sid
    {
        return Err(PlatformError::SecurityPolicy);
    }
    use windows_permissions::wrappers::ConvertSecurityDescriptorToStringSecurityDescriptor;
    let sddl =
        ConvertSecurityDescriptorToStringSecurityDescriptor(&descriptor, SecurityInformation::Dacl)
            .map_err(|source| platform_io("inspect_windows_dacl", source))?;
    if !sddl.to_string_lossy().starts_with("D:P") {
        return Err(PlatformError::SecurityPolicy);
    }
    Ok(())
}

#[cfg(windows)]
fn private_windows_descriptor()
-> Result<windows_permissions::LocalBox<windows_permissions::SecurityDescriptor>, PlatformError> {
    use windows_permissions::{LocalBox, SecurityDescriptor};

    let sddl = format!("D:P(A;;FA;;;{})", current_windows_user_sid()?);
    let descriptor: LocalBox<SecurityDescriptor> =
        sddl.parse().map_err(|_| PlatformError::SecurityPolicy)?;
    Ok(descriptor)
}

#[cfg(windows)]
fn current_windows_user_sid() -> Result<String, PlatformError> {
    use nt_token::OwnedToken;
    use windows::Win32::Security::TOKEN_QUERY;

    OwnedToken::from_current_process(TOKEN_QUERY)
        .map_err(|_| PlatformError::SecurityPolicy)?
        .user()
        .and_then(|sid| sid.to_string())
        .map_err(|_| PlatformError::SecurityPolicy)
}

#[cfg(any(unix, windows))]
fn platform_io(operation: &'static str, source: io::Error) -> PlatformError {
    PlatformError::Io { operation, source }
}

#[cfg(any(unix, windows))]
fn platform_error_to_io(error: PlatformError) -> io::Error {
    match error {
        PlatformError::Io { source, .. } => source,
        PlatformError::InvalidName => {
            io::Error::new(io::ErrorKind::InvalidInput, "private-tree name is invalid")
        }
        PlatformError::UnsupportedPlatform => io::Error::new(
            io::ErrorKind::Unsupported,
            "private-tree platform boundary is unsupported",
        ),
        PlatformError::InsecureParent | PlatformError::SecurityPolicy => io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private-tree security policy failed",
        ),
        PlatformError::ResourceLimit => io::Error::new(
            io::ErrorKind::InvalidData,
            "private-tree resource limit was exceeded",
        ),
    }
}

#[cfg(not(any(unix, windows)))]
fn unsupported_io() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "private-tree platform boundary is unsupported",
    )
}

#[derive(Debug)]
pub(crate) enum PublishFailure {
    NotCommitted {
        directory: Box<Directory>,
        source: PlatformError,
    },
    CommittedButDurabilityUnknown {
        directory: Box<PublishedDirectory>,
        source: io::Error,
    },
}

#[cfg(all(test, windows))]
pub(crate) fn protect_test_parent_path(path: &Path) -> Result<(), PlatformError> {
    use windows_permissions::{
        constants::{SeObjectType, SecurityInformation},
        wrappers::SetNamedSecurityInfo,
    };

    let descriptor = private_windows_descriptor()?;
    let dacl = descriptor.dacl().ok_or(PlatformError::SecurityPolicy)?;
    SetNamedSecurityInfo(
        path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        Some(dacl),
        None,
    )
    .map_err(|source| platform_io("protect_test_parent", source))
}
