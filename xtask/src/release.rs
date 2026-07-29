//! Manual release version planning from the repository's immutable tag history.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use semver::{BuildMetadata, Prerelease, Version};
use serde::Serialize;

const MAX_TAG_FILE_BYTES: u64 = 128 * 1024;
const MAX_TAGS: usize = 2_048;
const MAX_TAG_BYTES: usize = 128;

#[derive(Debug)]
pub(crate) struct Options {
    channel: Channel,
    tags: PathBuf,
    exact_version: Option<String>,
    output: PathBuf,
}

impl Options {
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, ReleasePlanError> {
        let mut channel = None;
        let mut tags = None;
        let mut exact_version = None;
        let mut output = None;

        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| ReleasePlanError::MissingValue(flag.clone()))?;
            match flag.as_str() {
                "--channel" => assign_once(
                    &mut channel,
                    Channel::parse(&value)?,
                    ReleasePlanError::DuplicateFlag(flag),
                )?,
                "--tags" => assign_once(
                    &mut tags,
                    PathBuf::from(value),
                    ReleasePlanError::DuplicateFlag(flag),
                )?,
                "--exact-version" => assign_once(
                    &mut exact_version,
                    value,
                    ReleasePlanError::DuplicateFlag(flag),
                )?,
                "--output" => assign_once(
                    &mut output,
                    PathBuf::from(value),
                    ReleasePlanError::DuplicateFlag(flag),
                )?,
                _ => return Err(ReleasePlanError::UnknownFlag(flag)),
            }
        }

        Ok(Self {
            channel: channel.ok_or(ReleasePlanError::MissingFlag("--channel"))?,
            tags: tags.ok_or(ReleasePlanError::MissingFlag("--tags"))?,
            exact_version,
            output: output.ok_or(ReleasePlanError::MissingFlag("--output"))?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    Alpha,
    Final,
}

impl Channel {
    fn parse(value: &str) -> Result<Self, ReleasePlanError> {
        match value {
            "alpha" => Ok(Self::Alpha),
            "final" => Ok(Self::Final),
            _ => Err(ReleasePlanError::InvalidChannel),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct ReleasePlan {
    version: String,
    tag: String,
    prerelease: bool,
    npm_tag: &'static str,
}

#[derive(Debug)]
struct ReleaseTag {
    version: Version,
    alpha_number: Option<u64>,
}

pub(crate) fn build(options: &Options) -> Result<(), ReleasePlanError> {
    let tags = read_tags(&options.tags)?;
    let plan = plan_release(options.channel, options.exact_version.as_deref(), &tags)?;
    let mut encoded = serde_json::to_vec_pretty(&plan).map_err(ReleasePlanError::Encode)?;
    encoded.push(b'\n');
    persist_new(&options.output, &encoded)?;
    println!("{}", plan.tag);
    Ok(())
}

fn plan_release(
    channel: Channel,
    exact_version: Option<&str>,
    tags: &[String],
) -> Result<ReleasePlan, ReleasePlanError> {
    if channel == Channel::Alpha && exact_version.is_some() {
        return Err(ReleasePlanError::AlphaExactVersion);
    }

    let releases = tags
        .iter()
        .map(|tag| parse_release_tag(tag))
        .collect::<Result<Vec<_>, _>>()?;
    let released_versions = releases
        .iter()
        .map(|release| release.version.clone())
        .collect::<BTreeSet<_>>();
    let stable_versions = releases
        .iter()
        .filter(|release| release.alpha_number.is_none())
        .map(|release| release.version.clone())
        .collect::<BTreeSet<_>>();
    let latest_stable = stable_versions.iter().next_back().cloned();
    let pending_alphas = pending_alpha_series(&releases, &stable_versions, latest_stable.as_ref());

    let version = match channel {
        Channel::Alpha => next_alpha(latest_stable.as_ref(), &pending_alphas)?,
        Channel::Final => match exact_version {
            Some(exact) => exact_final(
                exact,
                latest_stable.as_ref(),
                pending_alphas.keys().next_back(),
                &released_versions,
            )?,
            None => next_final(latest_stable.as_ref(), &pending_alphas)?,
        },
    };
    if released_versions.contains(&version) {
        return Err(ReleasePlanError::AlreadyReleased(version.to_string()));
    }

    let prerelease = channel == Channel::Alpha;
    Ok(ReleasePlan {
        tag: format!("v{version}"),
        version: version.to_string(),
        prerelease,
        npm_tag: if prerelease { "alpha" } else { "latest" },
    })
}

fn pending_alpha_series(
    releases: &[ReleaseTag],
    stable_versions: &BTreeSet<Version>,
    latest_stable: Option<&Version>,
) -> BTreeMap<Version, u64> {
    let mut pending: BTreeMap<Version, u64> = BTreeMap::new();
    for release in releases {
        let Some(alpha_number) = release.alpha_number else {
            continue;
        };
        let base = stable_version(&release.version);
        if stable_versions.contains(&base) || latest_stable.is_some_and(|stable| base <= *stable) {
            continue;
        }
        pending
            .entry(base)
            .and_modify(|current| *current = (*current).max(alpha_number))
            .or_insert(alpha_number);
    }
    pending
}

fn next_alpha(
    latest_stable: Option<&Version>,
    pending: &BTreeMap<Version, u64>,
) -> Result<Version, ReleasePlanError> {
    let (base, alpha_number) = match pending.last_key_value() {
        Some((base, alpha_number)) => (
            base.clone(),
            alpha_number
                .checked_add(1)
                .ok_or(ReleasePlanError::VersionOverflow)?,
        ),
        None => (next_base(latest_stable)?, 1),
    };
    with_alpha(base, alpha_number)
}

fn next_final(
    latest_stable: Option<&Version>,
    pending: &BTreeMap<Version, u64>,
) -> Result<Version, ReleasePlanError> {
    match pending.last_key_value() {
        Some((base, _)) => Ok(base.clone()),
        None => next_base(latest_stable),
    }
}

fn exact_final(
    raw: &str,
    latest_stable: Option<&Version>,
    highest_pending: Option<&Version>,
    released_versions: &BTreeSet<Version>,
) -> Result<Version, ReleasePlanError> {
    let normalized = raw.strip_prefix('v').unwrap_or(raw);
    let version = Version::parse(normalized)
        .map_err(|error| ReleasePlanError::InvalidExactVersion(error.to_string()))?;
    if version.to_string() != normalized || !version.pre.is_empty() || !version.build.is_empty() {
        return Err(ReleasePlanError::InvalidExactVersion(
            "exact final version must be canonical stable SemVer".to_owned(),
        ));
    }
    if released_versions.contains(&version) {
        return Err(ReleasePlanError::AlreadyReleased(version.to_string()));
    }
    if latest_stable.is_some_and(|stable| version <= *stable) {
        return Err(ReleasePlanError::VersionRegression);
    }
    if highest_pending.is_some_and(|pending| version < *pending) {
        return Err(ReleasePlanError::VersionRegression);
    }
    Ok(version)
}

fn next_base(latest_stable: Option<&Version>) -> Result<Version, ReleasePlanError> {
    match latest_stable {
        Some(stable) => Ok(Version::new(
            stable.major,
            stable
                .minor
                .checked_add(1)
                .ok_or(ReleasePlanError::VersionOverflow)?,
            0,
        )),
        None => Ok(Version::new(0, 1, 0)),
    }
}

fn with_alpha(mut version: Version, number: u64) -> Result<Version, ReleasePlanError> {
    version.pre = Prerelease::new(&format!("alpha.{number}"))
        .map_err(|error| ReleasePlanError::InvalidTag(error.to_string()))?;
    Ok(version)
}

fn stable_version(version: &Version) -> Version {
    let mut stable = version.clone();
    stable.pre = Prerelease::EMPTY;
    stable.build = BuildMetadata::EMPTY;
    stable
}

fn parse_release_tag(tag: &str) -> Result<ReleaseTag, ReleasePlanError> {
    let raw = tag
        .strip_prefix('v')
        .ok_or_else(|| ReleasePlanError::InvalidTag(tag.to_owned()))?;
    let version =
        Version::parse(raw).map_err(|error| ReleasePlanError::InvalidTag(error.to_string()))?;
    if version.to_string() != raw || !version.build.is_empty() {
        return Err(ReleasePlanError::InvalidTag(tag.to_owned()));
    }
    let alpha_number = if version.pre.is_empty() {
        None
    } else {
        let pre = version.pre.as_str();
        let number = pre
            .strip_prefix("alpha.")
            .ok_or_else(|| ReleasePlanError::InvalidTag(tag.to_owned()))?;
        if number.is_empty()
            || (number.len() > 1 && number.starts_with('0'))
            || !number.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(ReleasePlanError::InvalidTag(tag.to_owned()));
        }
        Some(
            number
                .parse::<u64>()
                .map_err(|_| ReleasePlanError::InvalidTag(tag.to_owned()))?,
        )
    };
    Ok(ReleaseTag {
        version,
        alpha_number,
    })
}

fn read_tags(path: &Path) -> Result<Vec<String>, ReleasePlanError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| ReleasePlanError::InputIo {
        path: path.to_path_buf(),
        error,
    })?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_TAG_FILE_BYTES {
        return Err(ReleasePlanError::InvalidTagFile);
    }
    let text = fs::read_to_string(path).map_err(|error| ReleasePlanError::InputIo {
        path: path.to_path_buf(),
        error,
    })?;
    let tags = text.lines().map(str::to_owned).collect::<Vec<_>>();
    if tags.len() > MAX_TAGS
        || tags.iter().any(|tag| {
            tag.is_empty()
                || tag.len() > MAX_TAG_BYTES
                || !tag.bytes().all(|byte| byte.is_ascii_graphic())
        })
    {
        return Err(ReleasePlanError::InvalidTagFile);
    }
    Ok(tags)
}

fn persist_new(path: &Path, bytes: &[u8]) -> Result<(), ReleasePlanError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| ReleasePlanError::OutputIo {
            path: path.to_path_buf(),
            error,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| ReleasePlanError::OutputIo {
            path: path.to_path_buf(),
            error,
        })
}

fn assign_once<T>(
    slot: &mut Option<T>,
    value: T,
    duplicate: ReleasePlanError,
) -> Result<(), ReleasePlanError> {
    if slot.replace(value).is_some() {
        return Err(duplicate);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReleasePlanError {
    #[error("missing required release planning flag: {0}")]
    MissingFlag(&'static str),
    #[error("release planning flag requires a value: {0}")]
    MissingValue(String),
    #[error("duplicate release planning flag: {0}")]
    DuplicateFlag(String),
    #[error("unknown release planning flag: {0}")]
    UnknownFlag(String),
    #[error("release channel must be alpha or final")]
    InvalidChannel,
    #[error("an exact version is supported only for final releases")]
    AlphaExactVersion,
    #[error("release tag is invalid: {0}")]
    InvalidTag(String),
    #[error("exact final version is invalid: {0}")]
    InvalidExactVersion(String),
    #[error("release version has already been published: {0}")]
    AlreadyReleased(String),
    #[error("exact final version would regress the release history")]
    VersionRegression,
    #[error("release version arithmetic overflowed")]
    VersionOverflow,
    #[error("release tag input must be a bounded regular UTF-8 file")]
    InvalidTagFile,
    #[error("failed to read release tag input: {path}")]
    InputIo {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to write release plan: {path}")]
    OutputIo {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to encode release plan")]
    Encode(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(channel: Channel, exact: Option<&str>, tags: &[&str]) -> ReleasePlan {
        plan_release(
            channel,
            exact,
            &tags.iter().map(|tag| (*tag).to_owned()).collect::<Vec<_>>(),
        )
        .expect("release plan succeeds")
    }

    #[test]
    fn alpha_sequence_promotes_and_advances_after_final() {
        assert_eq!(plan(Channel::Alpha, None, &[]).tag, "v0.1.0-alpha.1");
        assert_eq!(
            plan(Channel::Alpha, None, &["v0.1.0-alpha.1"]).tag,
            "v0.1.0-alpha.2"
        );
        assert_eq!(
            plan(
                Channel::Final,
                None,
                &["v0.1.0-alpha.1", "v0.1.0-alpha.2", "v0.1.0-alpha.3",],
            )
            .tag,
            "v0.1.0"
        );
        assert_eq!(
            plan(Channel::Alpha, None, &["v0.1.0-alpha.1", "v0.1.0"],).tag,
            "v0.2.0-alpha.1"
        );
    }

    #[test]
    fn consecutive_finals_increment_the_minor_version() {
        assert_eq!(plan(Channel::Final, None, &["v0.1.0"]).tag, "v0.2.0");
        assert_eq!(plan(Channel::Final, None, &["v1.9.0"]).tag, "v1.10.0");
    }

    #[test]
    fn explicit_final_can_promote_or_jump_forward() {
        assert_eq!(
            plan(Channel::Final, Some("0.1.0"), &["v0.1.0-alpha.3"],).tag,
            "v0.1.0"
        );
        assert_eq!(
            plan(Channel::Final, Some("v0.130.0"), &["v0.1.0"]).tag,
            "v0.130.0"
        );
    }

    #[test]
    fn invalid_or_regressive_history_fails_closed() {
        assert!(plan_release(Channel::Alpha, Some("0.1.0"), &[]).is_err());
        assert!(plan_release(Channel::Final, Some("0.1.0"), &["v0.1.0".to_owned()]).is_err());
        assert!(
            plan_release(
                Channel::Final,
                Some("0.1.0"),
                &["v0.2.0-alpha.1".to_owned()]
            )
            .is_err()
        );
        assert!(plan_release(Channel::Alpha, None, &["v0.1.0-beta.1".to_owned()]).is_err());
        assert!(plan_release(Channel::Alpha, None, &["release-0.1.0".to_owned()]).is_err());
    }
}
