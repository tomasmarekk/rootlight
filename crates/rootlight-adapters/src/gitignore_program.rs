//! Compile source-owned Gitignore programs with the pinned native matcher.
//! Matching is confined to normalized relative paths in one rule-file context;
//! repository traversal, nested rule files and global Git configuration are separate.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SinkError};
use rootlight_cancel::Cancellation;
use rootlight_ir::{
    GitignorePolarity, GitignoreProgram, MAX_GITIGNORE_RULES, MAX_GITIGNORE_SOURCE_BYTES,
    SourceRef, SourceSpan,
};

/// Maximum UTF-8 bytes in one portable relative matching path.
pub const MAX_GITIGNORE_MATCH_PATH_BYTES: usize = 4096;
/// Maximum components in one relative matching path, including the leaf.
pub const MAX_GITIGNORE_MATCH_PATH_COMPONENTS: usize = 128;

/// The last matching source rule, or an excluded ancestor that prevents traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitignoreDecision {
    /// Zero-based index in the program's source-ordered rule vector.
    pub rule_index: usize,
    /// Whether the rule excludes or re-includes the matched entry.
    pub polarity: GitignorePolarity,
    /// Whether an ignored ancestor prevents reaching the requested path.
    pub blocked_by_parent: bool,
}

/// A validated single-file rule program and its compiled, case-sensitive matcher.
pub struct GitignoreMatcher {
    program: GitignoreProgram,
    matcher: Gitignore,
}

impl GitignoreMatcher {
    /// Parses complete physical UTF-8 source using the pinned Gitignore grammar.
    ///
    /// Source identity, ordered rule ownership and native lexical metadata are
    /// authenticated by the IR constructor. Invalid globs refuse the whole program.
    ///
    /// # Errors
    /// Returns an error for invalid source identity, invalid globs, hard limits,
    /// cancellation, matcher construction or allocation failure.
    pub fn parse(
        source: SourceRef,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        if text.len() > MAX_GITIGNORE_SOURCE_BYTES {
            return Err(failure("gitignore-source-byte-limit"));
        }
        let mut builder = GitignoreBuilder::new("");
        let mut lines = Vec::new();
        let mut offset = 0_usize;
        for (index, raw) in text.split_inclusive('\n').enumerate() {
            cancellation.check()?;
            let line = if let Some(line) = raw.strip_suffix('\n') {
                line.strip_suffix('\r').unwrap_or(line)
            } else {
                raw
            };
            let effective = if index == 0 {
                line.trim_start_matches('\u{feff}')
            } else {
                line
            };
            let normalized = original(effective);
            if !normalized.is_empty() && !normalized.starts_with('#') {
                if lines.len() >= MAX_GITIGNORE_RULES {
                    return Err(failure("gitignore-rule-limit"));
                }
                builder
                    .add_line(None, effective)
                    .map_err(|_| failure("gitignore-pattern-parse-error"))?;
                lines
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let start = offset
                    .checked_add(line.len() - effective.len())
                    .ok_or_else(|| failure("gitignore-source-offset-limit"))?;
                let end = offset
                    .checked_add(line.len())
                    .ok_or_else(|| failure("gitignore-source-offset-limit"))?;
                lines.push(span(source.span().file(), start, end)?);
            }
            offset = offset
                .checked_add(raw.len())
                .ok_or_else(|| failure("gitignore-source-offset-limit"))?;
        }
        let program = GitignoreProgram::from_evidence(source, text, lines)
            .map_err(|_| failure("gitignore-program-evidence"))?;
        let matcher = builder
            .build()
            .map_err(|_| failure("gitignore-matcher-build-error"))?;
        cancellation.check()?;
        Ok(Self { program, matcher })
    }

    /// Compiles a canonically decoded, source-owned native program.
    ///
    /// # Errors
    /// Returns an error for invalid globs, cancellation or matcher construction.
    pub fn from_program(
        program: GitignoreProgram,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut builder = GitignoreBuilder::new("");
        for rule in program.rules() {
            cancellation.check()?;
            let line = slice(program.text(), rule.line)?;
            builder
                .add_line(None, line)
                .map_err(|_| failure("gitignore-pattern-parse-error"))?;
        }
        let matcher = builder
            .build()
            .map_err(|_| failure("gitignore-matcher-build-error"))?;
        cancellation.check()?;
        Ok(Self { program, matcher })
    }

    /// Returns exact source, ordered rules and their native lexical metadata.
    #[must_use]
    pub const fn program(&self) -> &GitignoreProgram {
        &self.program
    }

    /// Evaluates one normalized slash-separated path relative to this rule file.
    ///
    /// Ancestors are evaluated from the root before the leaf, so a negated child
    /// cannot be re-included beneath an excluded directory. No filesystem is read.
    ///
    /// # Errors
    /// Rejects absolute/drive-prefixed paths, backslashes, empty/dot/parent
    /// components, NUL, path/component limits, cancellation or an inconsistent
    /// compiled-rule inventory.
    pub fn decision(
        &self,
        relative_path: &str,
        is_directory: bool,
        cancellation: &Cancellation,
    ) -> Result<Option<GitignoreDecision>, AdapterError> {
        if relative_path.len() > MAX_GITIGNORE_MATCH_PATH_BYTES
            || relative_path.contains(['\\', '\0'])
            || (relative_path.as_bytes().get(1) == Some(&b':')
                && relative_path
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic))
            || relative_path.split('/').enumerate().any(|(index, part)| {
                index >= MAX_GITIGNORE_MATCH_PATH_COMPONENTS || matches!(part, "" | "." | "..")
            })
        {
            return Err(failure("gitignore-relative-path-invalid"));
        }
        for (offset, byte) in relative_path.bytes().enumerate() {
            if offset % 4096 == 0 {
                cancellation.check()?;
            }
            if byte == b'/' {
                cancellation.check()?;
                let parent = relative_path
                    .get(..offset)
                    .ok_or_else(|| failure("gitignore-relative-path-invalid"))?;
                if let Some(mut decision) = self.direct_decision(parent, true, cancellation)?
                    && decision.polarity == GitignorePolarity::Exclude
                {
                    decision.blocked_by_parent = true;
                    return Ok(Some(decision));
                }
            }
        }
        cancellation.check()?;
        self.direct_decision(relative_path, is_directory, cancellation)
    }

    fn direct_decision(
        &self,
        path: &str,
        is_directory: bool,
        cancellation: &Cancellation,
    ) -> Result<Option<GitignoreDecision>, AdapterError> {
        let matched = self.matcher.matched(path, is_directory);
        let glob = match matched {
            ignore::Match::None => return Ok(None),
            ignore::Match::Ignore(glob) | ignore::Match::Whitelist(glob) => glob,
        };
        // Identical duplicate spellings have identical matching semantics, and
        // the last authored copy owns the native last-match decision.
        for (rule_index, rule) in self.program.rules().iter().enumerate().rev() {
            if rule_index % 64 == 0 {
                cancellation.check()?;
            }
            if original(slice(self.program.text(), rule.line)?) == glob.original() {
                return Ok(Some(GitignoreDecision {
                    rule_index,
                    polarity: rule.polarity,
                    blocked_by_parent: false,
                }));
            }
        }
        Err(failure("gitignore-matcher-rule-identity"))
    }
}

fn original(line: &str) -> &str {
    if line.ends_with("\\ ") {
        line
    } else {
        line.trim_end()
    }
}

fn slice(text: &str, span: SourceSpan) -> Result<&str, AdapterError> {
    text.get(
        usize::try_from(span.start_byte()).map_err(|_| failure("gitignore-source-offset-limit"))?
            ..usize::try_from(span.end_byte())
                .map_err(|_| failure("gitignore-source-offset-limit"))?,
    )
    .ok_or_else(|| failure("gitignore-program-evidence"))
}

fn span(file: rootlight_ids::FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        file,
        u64::try_from(start).map_err(|_| failure("gitignore-source-offset-limit"))?,
        u64::try_from(end).map_err(|_| failure("gitignore-source-offset-limit"))?,
    )
    .map_err(|_| failure("gitignore-source-offset-limit"))
}

pub(super) fn failure(code: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("fixed Gitignore diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, GenerationId, RepositoryId, content_hash};

    fn parse(text: &str) -> GitignoreMatcher {
        let file = FileId::from_bytes([3; 20]);
        let source = SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(file, 0, u64::try_from(text.len()).unwrap()).unwrap(),
            content_hash(text.as_bytes()),
            None,
        );
        GitignoreMatcher::parse(source, text, &Cancellation::new()).unwrap()
    }

    fn decision(
        matcher: &GitignoreMatcher,
        path: &str,
        directory: bool,
    ) -> Option<GitignoreDecision> {
        matcher
            .decision(path, directory, &Cancellation::new())
            .unwrap()
    }

    #[test]
    fn native_order_negation_and_duplicate_ownership() {
        let matcher = parse("*.log\n!important.log\n*.log\n");
        let matched = decision(&matcher, "important.log", false).unwrap();
        assert_eq!(matched.rule_index, 2);
        assert_eq!(matched.polarity, GitignorePolarity::Exclude);
        let matcher = parse("*.log\n!important.log\n");
        assert_eq!(
            decision(&matcher, "sub/important.log", false)
                .unwrap()
                .polarity,
            GitignorePolarity::Reinclude
        );
        assert!(decision(&matcher, "source.rs", false).is_none());
    }

    #[test]
    fn negated_child_cannot_reinclude_under_excluded_parent() {
        let matcher = parse("dir/\n!dir/keep\n");
        let matched = decision(&matcher, "dir/keep", false).unwrap();
        assert_eq!(matched.rule_index, 0);
        assert!(matched.blocked_by_parent);
        assert_eq!(matched.polarity, GitignorePolarity::Exclude);
        let matcher = parse("dir/*\n!dir/keep\n");
        let matched = decision(&matcher, "dir/keep", false).unwrap();
        assert_eq!(matched.rule_index, 1);
        assert!(!matched.blocked_by_parent);
        assert_eq!(matched.polarity, GitignorePolarity::Reinclude);
    }

    #[test]
    fn native_anchoring_directory_globstar_and_escapes() {
        let matcher = parse("/root\ncache/\na/**/z\n\\!literal\n\\#hash\nfile\\ \n[abc\n");
        assert!(decision(&matcher, "root", false).is_some());
        assert!(decision(&matcher, "sub/root", false).is_none());
        assert!(decision(&matcher, "cache", false).is_none());
        assert!(
            decision(&matcher, "sub/cache/item", false)
                .unwrap()
                .blocked_by_parent
        );
        for path in ["a/z", "a/b/z", "!literal", "#hash", "file ", "[abc"] {
            assert!(decision(&matcher, path, false).is_some(), "{path}");
        }
    }

    #[test]
    fn physical_bom_crlf_and_decoded_program_preserve_semantics() {
        let matcher = parse("\u{feff}# comment\r\n/root\r\n!keep\r\n");
        assert_eq!(matcher.program().rules().len(), 2);
        let rebuilt =
            GitignoreMatcher::from_program(matcher.program().clone(), &Cancellation::new())
                .unwrap();
        assert_eq!(
            decision(&matcher, "root", false),
            decision(&rebuilt, "root", false)
        );
        for path in ["", "/root", "../root", "a/./b", "a\\b", "C:/root"] {
            assert!(
                rebuilt.decision(path, false, &Cancellation::new()).is_err(),
                "{path}"
            );
        }
        assert!(
            rebuilt
                .decision(
                    &"a".repeat(MAX_GITIGNORE_MATCH_PATH_BYTES + 1),
                    false,
                    &Cancellation::new()
                )
                .is_err()
        );
        assert!(
            rebuilt
                .decision(
                    &"a/".repeat(MAX_GITIGNORE_MATCH_PATH_COMPONENTS),
                    false,
                    &Cancellation::new()
                )
                .is_err()
        );
    }
}
