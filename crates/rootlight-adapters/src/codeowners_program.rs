//! Case-sensitive path decisions over authenticated CODEOWNERS rule programs.
//! Decisions describe authored owner intent for one explicitly selected file;
//! repository file precedence and GitHub account permissions are not inferred.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rootlight_adapter_sdk::{AdapterError, DiagnosticCode};
use rootlight_cancel::Cancellation;
use rootlight_ir::{CodeownersProgram, SourceRef, SourceSpan};

/// Maximum bytes in one normalized repository-relative matching path.
pub const MAX_CODEOWNERS_MATCH_PATH_BYTES: usize = 4096;
/// Maximum components in one normalized repository-relative matching path.
pub const MAX_CODEOWNERS_MATCH_PATH_COMPONENTS: usize = 128;

/// The last matching rule's authored intent, including an empty clearing list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeownersDecision {
    /// Zero-based source-ordered rule index in the authenticated program.
    pub rule_index: usize,
}

/// A native program with a bounded, case-sensitive path matcher.
pub struct CodeownersMatcher {
    program: CodeownersProgram,
    matcher: Gitignore,
}

impl CodeownersMatcher {
    /// Parses authenticated UTF-8 source and compiles the supported path grammar.
    ///
    /// Invalid physical lines remain in the program and do not enter matching.
    ///
    /// # Errors
    /// Rejects foreign source identity, source/rule limits, cancellation, native
    /// compiler failures and pattern semantics outside [`Self::from_program`].
    pub fn parse(
        source: SourceRef,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let program = CodeownersProgram::from_source(source, text)
            .map_err(|_| failure("codeowners-program-evidence"))?;
        Self::from_program(program, cancellation)
    }

    /// Compiles ordered CODEOWNERS patterns without Gitignore parent exclusion.
    ///
    /// Literal paths and trailing-directory patterns include descendants. A
    /// terminal wildcard matches only its own path depth (`docs/*` does not
    /// match `docs/nested/file`). Slash-free names match at any depth. The pinned
    /// matcher supplies `*`, `?`, complete-component `**` and literal escapes.
    /// Braces and non-component globstars are refused rather than interpreted
    /// using the glob compiler's broader syntax. Invalid lines are skipped.
    /// This evaluates one chosen CODEOWNERS file; it does not choose between
    /// `.github/CODEOWNERS`, `CODEOWNERS` and `docs/CODEOWNERS`, or prove account
    /// existence, write permissions, branch policy or review assignment.
    ///
    /// # Errors
    /// Returns cancellation, compiler failure, or
    /// `codeowners-pattern-semantics-unavailable` for an unsupported pattern.
    pub fn from_program(
        program: CodeownersProgram,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut builder = GitignoreBuilder::new("");
        for rule in program.rules() {
            cancellation.check()?;
            let pattern = slice(program.text(), rule.pattern)?;
            ensure_matching_syntax(pattern)?;
            builder
                .add_line(None, pattern)
                .map_err(|_| failure("codeowners-pattern-semantics-unavailable"))?;
            if let Some(descendants) = descendants_pattern(pattern) {
                builder
                    .add_line(None, &descendants)
                    .map_err(|_| failure("codeowners-pattern-semantics-unavailable"))?;
            }
        }
        let matcher = builder
            .build()
            .map_err(|_| failure("codeowners-matcher-build-error"))?;
        cancellation.check()?;
        Ok(Self { program, matcher })
    }

    /// Returns the exact source, ordered rules, owner spans and invalid lines.
    #[must_use]
    pub const fn program(&self) -> &CodeownersProgram {
        &self.program
    }

    /// Finds the last matching rule for a normalized repository-relative path.
    ///
    /// An empty rule owner vector clears preceding intent. Callers read the
    /// selected rule's owners from [`Self::program`]; no owner is resolved to a
    /// GitHub identity and no filesystem or network operation occurs.
    ///
    /// # Errors
    /// Rejects absolute, drive-prefixed, backslash, NUL, empty/dot/parent paths,
    /// path/component bounds, cancellation and inconsistent matcher identity.
    pub fn decision(
        &self,
        relative_path: &str,
        is_directory: bool,
        cancellation: &Cancellation,
    ) -> Result<Option<CodeownersDecision>, AdapterError> {
        cancellation.check()?;
        if relative_path.len() > MAX_CODEOWNERS_MATCH_PATH_BYTES
            || relative_path.contains(['\\', '\0'])
            || (relative_path.as_bytes().get(1) == Some(&b':')
                && relative_path
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic))
            || relative_path.split('/').enumerate().any(|(index, part)| {
                index >= MAX_CODEOWNERS_MATCH_PATH_COMPONENTS || matches!(part, "" | "." | "..")
            })
        {
            return Err(failure("codeowners-relative-path-invalid"));
        }
        let glob = match self.matcher.matched(relative_path, is_directory) {
            ignore::Match::None => return Ok(None),
            ignore::Match::Ignore(glob) => glob,
            ignore::Match::Whitelist(_) => return Err(failure("codeowners-matcher-rule-identity")),
        };
        for (rule_index, rule) in self.program.rules().iter().enumerate().rev() {
            cancellation.check()?;
            let pattern = slice(self.program.text(), rule.pattern)?;
            if glob.original() == pattern
                || descendants_pattern(pattern).as_deref() == Some(glob.original())
            {
                return Ok(Some(CodeownersDecision { rule_index }));
            }
        }
        Err(failure("codeowners-matcher-rule-identity"))
    }
}

pub(super) fn ensure_matching_syntax(pattern: &str) -> Result<(), AdapterError> {
    let mut escaped = false;
    for component in pattern.split('/') {
        if component.contains("**") && component != "**" {
            return Err(failure("codeowners-pattern-semantics-unavailable"));
        }
    }
    for character in pattern.chars() {
        if escaped {
            if character == '/' {
                return Err(failure("codeowners-pattern-semantics-unavailable"));
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if matches!(character, '{' | '}') {
            return Err(failure("codeowners-pattern-semantics-unavailable"));
        }
    }
    Ok(())
}

fn descendants_pattern(pattern: &str) -> Option<String> {
    let directory = pattern.ends_with('/');
    let body = pattern.strip_suffix('/').unwrap_or(pattern);
    let terminal = body.rsplit('/').next()?;
    if !directory && has_unescaped_wildcard(terminal) {
        return None;
    }
    let prefix = if body.contains('/') { "" } else { "**/" };
    Some(format!("{prefix}{body}/**"))
}

fn has_unescaped_wildcard(pattern: &str) -> bool {
    let mut escaped = false;
    for character in pattern.chars() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if matches!(character, '*' | '?') {
            return true;
        }
    }
    false
}

fn slice(text: &str, span: SourceSpan) -> Result<&str, AdapterError> {
    text.get(
        usize::try_from(span.start_byte()).map_err(|_| failure("codeowners-source-offset-limit"))?
            ..usize::try_from(span.end_byte())
                .map_err(|_| failure("codeowners-source-offset-limit"))?,
    )
    .ok_or_else(|| failure("codeowners-source-offset-limit"))
}

pub(super) fn failure(code: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("fixed CODEOWNERS diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, GenerationId, RepositoryId, content_hash};

    fn parse(text: &str) -> CodeownersMatcher {
        let source = SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(
                FileId::from_bytes([3; 20]),
                0,
                text.len().try_into().unwrap(),
            )
            .unwrap(),
            content_hash(text.as_bytes()),
            None,
        );
        CodeownersMatcher::parse(source, text, &Cancellation::new()).unwrap()
    }

    fn decision(matcher: &CodeownersMatcher, path: &str) -> Option<usize> {
        matcher
            .decision(path, false, &Cancellation::new())
            .unwrap()
            .map(|decision| decision.rule_index)
    }

    #[test]
    fn documented_order_duplicates_and_empty_owner_clearing_are_native() {
        let matcher =
            parse("* @global\n/apps/ @team/dev\n/apps/github\n*.js @first\n*.js @last # comment\n");
        assert_eq!(decision(&matcher, "README.md"), Some(0));
        assert_eq!(decision(&matcher, "apps/service/file.rs"), Some(1));
        assert_eq!(decision(&matcher, "apps/github/nested/file.rs"), Some(2));
        assert!(matcher.program().rules()[2].owners.is_empty());
        assert_eq!(decision(&matcher, "apps/github/file.js"), Some(4));
        assert_eq!(decision(&matcher, "nested/FILE.JS"), Some(0));
    }

    #[test]
    fn documented_directory_anchoring_and_terminal_star_depth_are_distinct() {
        let matcher = parse("docs/* @docs\n/build/logs/ @build\napps/ @apps\n**/logs @logs\n");
        assert_eq!(decision(&matcher, "docs/getting-started.md"), Some(0));
        assert_eq!(
            decision(&matcher, "docs/build-app/troubleshooting.md"),
            None
        );
        assert_eq!(decision(&matcher, "sub/docs/getting-started.md"), None);
        assert_eq!(decision(&matcher, "build/logs/nested/file"), Some(3));
        assert_eq!(decision(&matcher, "sub/build/logs/file"), Some(3));
        assert_eq!(decision(&matcher, "sub/apps/nested/file"), Some(2));
        assert_eq!(decision(&matcher, "deeply/nested/logs/file"), Some(3));
        assert_eq!(decision(&matcher, "Apps/file"), None);
    }

    #[test]
    fn invalid_lines_are_skipped_and_escape_unicode_and_globstar_are_exact() {
        let matcher = parse(
            "* @all\n!secret @negated\n[a-z] @range\n\\#hash @hash\n*.rs not-an-owner\na/**/z @deep\n/λ/ @unicode\nfile\\ name @space\n",
        );
        assert_eq!(matcher.program().invalid_lines().len(), 4);
        assert_eq!(decision(&matcher, "a/z"), Some(1));
        assert_eq!(decision(&matcher, "a/b/c/z"), Some(1));
        assert_eq!(decision(&matcher, "λ/file"), Some(2));
        assert_eq!(decision(&matcher, "file name"), Some(3));
        assert_eq!(decision(&matcher, "secret"), Some(0));
        for path in ["", "/absolute", "a/../b", "a//b", "C:/drive", "a\\b"] {
            assert!(matcher.decision(path, false, &Cancellation::new()).is_err());
        }
        for text in ["a{b,c} @owner\n", "a**b @owner\n", "foo\\/bar @owner\n"] {
            let program = parse("* @owner\n").program;
            let source = SourceRef::new(
                program.source().repository(),
                program.source().generation(),
                SourceSpan::new(
                    program.source().span().file(),
                    0,
                    text.len().try_into().unwrap(),
                )
                .unwrap(),
                content_hash(text.as_bytes()),
                None,
            );
            assert!(CodeownersMatcher::parse(source, text, &Cancellation::new()).is_err());
        }
    }

    #[test]
    fn escaped_terminal_metacharacters_keep_literal_directory_inheritance() {
        let matcher = parse("literal\\* @star\nliteral\\? @question\n");
        assert_eq!(decision(&matcher, "literal*/child.txt"), Some(0));
        assert_eq!(decision(&matcher, "sub/literal*/child.txt"), Some(0));
        assert_eq!(decision(&matcher, "literal?/child.txt"), Some(1));
        assert_eq!(decision(&matcher, "literalX/child.txt"), None);
    }
}
