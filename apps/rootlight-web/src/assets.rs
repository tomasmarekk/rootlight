//! Verified in-memory inventory for immutable production web assets.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use axum::body::Bytes;
use data_encoding::HEXLOWER;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::error::WebError;

const MANIFEST_NAME: &str = "asset-manifest.json";
const MAX_ASSETS: usize = 1_024;
const MAX_ASSET_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_ASSET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ASSET_PATH_BYTES: usize = 512;

#[derive(Clone)]
pub(crate) struct AssetInventory {
    assets: BTreeMap<String, Asset>,
}

#[derive(Clone)]
pub(crate) struct Asset {
    pub(crate) bytes: Bytes,
    pub(crate) content_type: String,
    pub(crate) immutable: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetManifest {
    schema_version: u32,
    assets: Vec<AssetRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetRecord {
    path: String,
    bytes: u64,
    sha256: String,
}

impl AssetInventory {
    pub(crate) fn load(root: &Path) -> Result<Self, WebError> {
        validate_asset_root(root)?;
        let manifest_path = root.join(MANIFEST_NAME);
        let manifest_metadata =
            fs::symlink_metadata(&manifest_path).map_err(|_| WebError::AssetsUnavailable)?;
        if !manifest_metadata.file_type().is_file()
            || manifest_metadata.file_type().is_symlink()
            || manifest_metadata.len() > MAX_ASSET_BYTES
        {
            return Err(WebError::AssetsUnavailable);
        }
        let manifest: AssetManifest = serde_json::from_slice(
            &fs::read(&manifest_path).map_err(|_| WebError::AssetsUnavailable)?,
        )
        .map_err(|_| WebError::AssetsUnavailable)?;
        if manifest.schema_version != 1
            || manifest.assets.is_empty()
            || manifest.assets.len() > MAX_ASSETS
        {
            return Err(WebError::AssetsUnavailable);
        }
        let mut assets = BTreeMap::new();
        let mut total_bytes = 0_u64;
        for record in manifest.assets {
            validate_asset_path(&record.path)?;
            let immutable = validate_cache_policy(&record.path)?;
            if record.bytes > MAX_ASSET_BYTES {
                return Err(WebError::AssetsUnavailable);
            }
            total_bytes = total_bytes
                .checked_add(record.bytes)
                .filter(|bytes| *bytes <= MAX_TOTAL_ASSET_BYTES)
                .ok_or(WebError::AssetsUnavailable)?;
            let source = root.join(PathBuf::from(&record.path));
            let metadata = validate_asset_source(root, &record.path)?;
            if !metadata.file_type().is_file() || metadata.len() != record.bytes {
                return Err(WebError::AssetsUnavailable);
            }
            let bytes = fs::read(&source).map_err(|_| WebError::AssetsUnavailable)?;
            if HEXLOWER.encode(Sha256::digest(&bytes).as_ref()) != record.sha256 {
                return Err(WebError::AssetsUnavailable);
            }
            let content_type = mime_guess::from_path(&record.path)
                .first_raw()
                .unwrap_or("application/octet-stream")
                .to_owned();
            if assets
                .insert(
                    record.path,
                    Asset {
                        bytes: Bytes::from(bytes),
                        content_type,
                        immutable,
                    },
                )
                .is_some()
            {
                return Err(WebError::AssetsUnavailable);
            }
        }
        if !assets.contains_key("index.html") {
            return Err(WebError::AssetsUnavailable);
        }
        Ok(Self { assets })
    }

    pub(crate) fn get(&self, path: &str) -> Option<&Asset> {
        self.assets.get(path)
    }

    pub(crate) fn index(&self) -> &Asset {
        self.assets
            .get("index.html")
            .expect("validated inventory always contains index.html")
    }
}

fn validate_asset_path(path: &str) -> Result<(), WebError> {
    if path.is_empty()
        || path.len() > MAX_ASSET_PATH_BYTES
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(WebError::AssetsUnavailable);
    }
    Ok(())
}

fn validate_asset_root(root: &Path) -> Result<(), WebError> {
    let metadata = fs::symlink_metadata(root).map_err(|_| WebError::AssetsUnavailable)?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err(WebError::AssetsUnavailable);
    }
    Ok(())
}

fn validate_asset_source(root: &Path, path: &str) -> Result<fs::Metadata, WebError> {
    let mut current = root.to_path_buf();
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|_| WebError::AssetsUnavailable)?;
        if is_link_or_reparse(&metadata)
            || (components.peek().is_some() && !metadata.file_type().is_dir())
        {
            return Err(WebError::AssetsUnavailable);
        }
        if components.peek().is_none() {
            return Ok(metadata);
        }
    }
    Err(WebError::AssetsUnavailable)
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn validate_cache_policy(path: &str) -> Result<bool, WebError> {
    if path == "index.html" {
        return Ok(false);
    }
    let file_name = path
        .strip_prefix("assets/")
        .filter(|name| !name.contains('/'))
        .ok_or(WebError::AssetsUnavailable)?;
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _extension)| stem)
        .unwrap_or(file_name);
    let hash = stem
        .rsplit_once('-')
        .map(|(_name, hash)| hash)
        .unwrap_or(stem);
    if hash.len() < 8 || !hash.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(WebError::AssetsUnavailable);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn inventory_verifies_hashes_and_rejects_traversal() {
        let root = TempDir::new().expect("asset root exists");
        let index = b"<!doctype html><html></html>";
        fs::write(root.path().join("index.html"), index).expect("index writes");
        write_manifest(
            root.path(),
            vec![json!({
                "path": "index.html",
                "bytes": index.len(),
                "sha256": HEXLOWER.encode(Sha256::digest(index).as_ref())
            })],
        );
        let inventory = AssetInventory::load(root.path()).expect("inventory validates");
        assert_eq!(inventory.index().bytes.as_ref(), index);
        assert!(!inventory.index().immutable);

        write_manifest(
            root.path(),
            vec![json!({
                "path": "../index.html",
                "bytes": index.len(),
                "sha256": HEXLOWER.encode(Sha256::digest(index).as_ref())
            })],
        );
        assert!(matches!(
            AssetInventory::load(root.path()),
            Err(WebError::AssetsUnavailable)
        ));
    }

    #[test]
    fn inventory_rejects_tampered_content() {
        let root = TempDir::new().expect("asset root exists");
        fs::write(root.path().join("index.html"), b"tampered").expect("index writes");
        write_manifest(
            root.path(),
            vec![json!({
                "path": "index.html",
                "bytes": 8,
                "sha256": "00".repeat(32)
            })],
        );
        assert!(matches!(
            AssetInventory::load(root.path()),
            Err(WebError::AssetsUnavailable)
        ));
    }

    #[test]
    fn inventory_requires_content_named_assets_for_immutable_caching() {
        let root = TempDir::new().expect("asset root exists");
        let index = b"<!doctype html><html></html>";
        let script = b"export {};";
        fs::create_dir(root.path().join("assets")).expect("asset directory creates");
        fs::write(root.path().join("index.html"), index).expect("index writes");
        fs::write(root.path().join("assets/app.js"), script).expect("script writes");
        write_manifest(
            root.path(),
            vec![
                asset_record("index.html", index),
                asset_record("assets/app.js", script),
            ],
        );
        assert!(matches!(
            AssetInventory::load(root.path()),
            Err(WebError::AssetsUnavailable)
        ));

        fs::rename(
            root.path().join("assets/app.js"),
            root.path().join("assets/app-a1b2c3d4.js"),
        )
        .expect("content-named script renames");
        write_manifest(
            root.path(),
            vec![
                asset_record("index.html", index),
                asset_record("assets/app-a1b2c3d4.js", script),
            ],
        );
        let inventory = AssetInventory::load(root.path()).expect("hashed inventory validates");
        assert!(
            inventory
                .get("assets/app-a1b2c3d4.js")
                .expect("script exists")
                .immutable
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn inventory_rejects_linked_asset_directories() {
        let root = TempDir::new().expect("asset root exists");
        let external = TempDir::new().expect("external asset root exists");
        let index = b"<!doctype html><html></html>";
        let script = b"export {};";
        fs::write(root.path().join("index.html"), index).expect("index writes");
        fs::write(external.path().join("app-a1b2c3d4.js"), script).expect("external script writes");
        if !create_directory_link(external.path(), &root.path().join("assets")) {
            return;
        }
        write_manifest(
            root.path(),
            vec![
                asset_record("index.html", index),
                asset_record("assets/app-a1b2c3d4.js", script),
            ],
        );
        assert!(matches!(
            AssetInventory::load(root.path()),
            Err(WebError::AssetsUnavailable)
        ));
    }

    fn write_manifest(root: &Path, assets: Vec<serde_json::Value>) {
        let manifest = serde_json::to_vec(&json!({
            "schema_version": 1,
            "assets": assets
        }))
        .expect("manifest serializes");
        fs::write(root.join(MANIFEST_NAME), manifest).expect("manifest writes");
    }

    fn asset_record(path: &str, bytes: &[u8]) -> serde_json::Value {
        json!({
            "path": path,
            "bytes": bytes.len(),
            "sha256": HEXLOWER.encode(Sha256::digest(bytes).as_ref())
        })
    }

    #[cfg(unix)]
    fn create_directory_link(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).expect("directory symlink creates");
        true
    }

    #[cfg(windows)]
    fn create_directory_link(target: &Path, link: &Path) -> bool {
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1_314;

        match std::os::windows::fs::symlink_dir(target, link) {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => false,
            Err(error) => panic!("directory reparse point creates: {error}"),
        }
    }
}
