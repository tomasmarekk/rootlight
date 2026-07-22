//! Deterministic local-link validation for repository Markdown trees.
//!
//! The checker is path-agnostic so ignored development specifications can use
//! the same validation logic locally without becoming a public CI dependency.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Validates every local Markdown link below `root`.
///
/// URL, `mailto:`, data, and same-document fragment destinations are outside
/// this filesystem check. Diagnostics are reported relative to `root`, so they
/// never expose the checkout's machine-specific absolute path.
///
/// # Errors
///
/// Returns [`MarkdownLinkError`] when the root cannot be inspected, a Markdown
/// file cannot be read, or at least one local destination does not exist.
pub(crate) fn check(root: &Path) -> Result<(), MarkdownLinkError> {
    let metadata =
        fs::metadata(root).map_err(|source| MarkdownLinkError::InspectRoot { source })?;
    if !metadata.is_dir() {
        return Err(MarkdownLinkError::RootIsNotDirectory);
    }

    let mut files = Vec::new();
    collect_markdown_files(root, root, &mut files)?;
    files.sort();
    let file_count = files.len();

    let mut checked = 0_usize;
    let mut broken = Vec::new();
    for relative_path in files {
        let path = root.join(&relative_path);
        let contents =
            fs::read_to_string(&path).map_err(|source| MarkdownLinkError::ReadMarkdown {
                path: display_relative(&relative_path),
                source,
            })?;
        for destination in markdown_destinations(&contents) {
            let Some(local_path) = local_destination(destination) else {
                continue;
            };
            checked = checked.saturating_add(1);
            let resolved = if local_path.starts_with('/') {
                root.join(local_path.trim_start_matches('/'))
            } else {
                path.parent()
                    .unwrap_or(root)
                    .join(local_path.replace('/', std::path::MAIN_SEPARATOR_STR))
            };
            if !resolved.exists() {
                broken.push(BrokenLink {
                    source: display_relative(&relative_path),
                    line: line_number(&contents, destination.offset),
                    destination: destination.value.to_owned(),
                });
            }
        }
    }

    if broken.is_empty() {
        println!(
            "validated {checked} local Markdown links across {} files",
            file_count
        );
        return Ok(());
    }

    broken.sort();
    Err(MarkdownLinkError::BrokenLinks {
        count: broken.len(),
        details: broken
            .into_iter()
            .map(|link| format!("{}:{} -> {}", link.source, link.line, link.destination))
            .collect::<Vec<_>>()
            .join("; "),
    })
}

fn collect_markdown_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), MarkdownLinkError> {
    let entries = fs::read_dir(directory).map_err(|source| MarkdownLinkError::ReadDirectory {
        path: relative_to_root(root, directory),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| MarkdownLinkError::ReadDirectory {
            path: relative_to_root(root, directory),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| MarkdownLinkError::ReadDirectory {
                path: relative_to_root(root, directory),
                source,
            })?;
        if file_type.is_dir() {
            collect_markdown_files(root, &entry.path(), files)?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            files.push(relative_to_root(root, &entry.path()));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct MarkdownDestination<'a> {
    value: &'a str,
    offset: usize,
}

fn markdown_destinations(contents: &str) -> Vec<MarkdownDestination<'_>> {
    let mut destinations = Vec::new();
    let mut line_offset = 0_usize;
    let mut fence: Option<(u8, usize)> = None;
    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed.as_bytes().first().copied();
        let marker_len = marker.map_or(0, |byte| {
            trimmed
                .as_bytes()
                .iter()
                .take_while(|candidate| **candidate == byte)
                .count()
        });
        if matches!(marker, Some(b'`' | b'~')) && marker_len >= 3 {
            match fence {
                Some((active, minimum)) if marker == Some(active) && marker_len >= minimum => {
                    fence = None;
                }
                None => fence = marker.map(|byte| (byte, marker_len)),
                Some(_) => {}
            }
            line_offset = line_offset.saturating_add(line.len());
            continue;
        }
        if fence.is_none() {
            destinations.extend(destinations_in_line(line, line_offset));
        }
        line_offset = line_offset.saturating_add(line.len());
    }
    destinations
}

fn destinations_in_line(line: &str, line_offset: usize) -> Vec<MarkdownDestination<'_>> {
    let mut destinations = Vec::new();
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    let mut inline_ticks = 0_usize;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let run = bytes[index..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            if inline_ticks == 0 {
                inline_ticks = run;
            } else if inline_ticks == run {
                inline_ticks = 0;
            }
            index = index.saturating_add(run);
            continue;
        }
        if inline_ticks == 0
            && bytes[index] == b']'
            && bytes.get(index.saturating_add(1)) == Some(&b'(')
        {
            let open = index.saturating_add(2);
            let Some(relative_close) = line[open..].find(')') else {
                break;
            };
            let close = open.saturating_add(relative_close);
            let raw = line[open..close].trim();
            let value = if let Some(stripped) = raw.strip_prefix('<') {
                stripped
                    .split_once('>')
                    .map_or(stripped, |(destination, _)| destination)
            } else {
                raw.split_ascii_whitespace().next().unwrap_or_default()
            };
            if !value.is_empty() {
                destinations.push(MarkdownDestination {
                    value,
                    offset: line_offset.saturating_add(open),
                });
            }
            index = close.saturating_add(1);
            continue;
        }
        index = index.saturating_add(1);
    }
    destinations
}

fn local_destination<'a>(destination: MarkdownDestination<'a>) -> Option<&'a str> {
    let value = destination.value;
    if value.starts_with('#')
        || value.starts_with("https://")
        || value.starts_with("http://")
        || value.starts_with("mailto:")
        || value.starts_with("data:")
    {
        return None;
    }
    let path = value.split_once('#').map_or(value, |(path, _)| path);
    (!path.is_empty()).then_some(path)
}

fn line_number(contents: &str, offset: usize) -> usize {
    contents
        .as_bytes()
        .get(..offset)
        .unwrap_or_default()
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        .saturating_add(1)
}

fn relative_to_root(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn display_relative(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BrokenLink {
    source: String,
    line: usize,
    destination: String,
}

/// Failure returned by the deterministic Markdown link checker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MarkdownLinkError {
    /// The requested root could not be inspected.
    #[error("failed to inspect Markdown root")]
    InspectRoot {
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The requested root exists but is not a directory.
    #[error("Markdown root is not a directory")]
    RootIsNotDirectory,
    /// One directory in the Markdown tree could not be read.
    #[error("failed to read Markdown directory {path}")]
    ReadDirectory {
        /// Root-relative directory path.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// One Markdown file could not be read as UTF-8 text.
    #[error("failed to read Markdown file {path}")]
    ReadMarkdown {
        /// Root-relative Markdown path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// One or more local link destinations do not exist.
    #[error("{count} broken local Markdown links: {details}")]
    BrokenLinks {
        /// Number of broken destinations.
        count: usize,
        /// Stable root-relative diagnostics.
        details: String,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{MarkdownLinkError, check};

    #[test]
    fn accepts_relative_fragments_and_external_destinations() {
        let root = tempdir().expect("temporary root is created");
        fs::create_dir(root.path().join("guide")).expect("guide directory is created");
        fs::write(root.path().join("guide/target.md"), "# Target\n").expect("target is written");
        fs::write(
            root.path().join("index.md"),
            "[local](guide/target.md#target)\n[web](https://example.com)\n[same](#section)\n",
        )
        .expect("source is written");

        check(root.path()).expect("all local destinations resolve");
    }

    #[test]
    fn reports_broken_links_without_absolute_checkout_paths() {
        let root = tempdir().expect("temporary root is created");
        fs::write(root.path().join("index.md"), "\n[missing](missing.md)\n")
            .expect("source is written");

        let error = check(root.path()).expect_err("missing destination is rejected");
        let MarkdownLinkError::BrokenLinks { count, details } = error else {
            panic!("expected broken-link error");
        };
        assert_eq!(count, 1);
        assert_eq!(details, "index.md:2 -> missing.md");
        assert!(!details.contains(&root.path().to_string_lossy().to_string()));
    }

    #[test]
    fn rejects_a_file_as_the_scan_root() {
        let root = tempdir().expect("temporary root is created");
        let file = root.path().join("index.md");
        fs::write(&file, "# Index\n").expect("source is written");

        assert!(matches!(
            check(&file),
            Err(MarkdownLinkError::RootIsNotDirectory)
        ));
    }

    #[test]
    fn ignores_link_syntax_inside_code() {
        let root = tempdir().expect("temporary root is created");
        fs::write(
            root.path().join("index.md"),
            "`[inline](missing-inline.md)`\n```\n[fenced](missing-fenced.md)\n```\n",
        )
        .expect("source is written");

        check(root.path()).expect("code examples are not treated as links");
    }
}
