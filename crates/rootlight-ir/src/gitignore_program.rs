//! Ordered Gitignore rule metadata over one complete immutable source file.
//! Lexical ownership is authenticated here; the native frontend's pinned glob
//! compiler separately validates matching syntax and supplies path semantics.

use std::io::{self, Write};

use rootlight_ids::{FactId, GenerationId, content_hash, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for ordered source-backed Gitignore programs.
pub const GITIGNORE_PROGRAM_NAMESPACE: &str = "rootlight.gitignore_program";
/// Canonical Gitignore program payload revision.
pub const GITIGNORE_PROGRAM_VERSION: &str = "1";
/// Maximum complete physical source size in bytes.
pub const MAX_GITIGNORE_SOURCE_BYTES: usize = 1024 * 1024;
/// Maximum active rules in one physical file, including duplicates.
pub const MAX_GITIGNORE_RULES: usize = 4096;
/// Maximum canonical payload size, including JSON escape expansion.
pub const MAX_GITIGNORE_PROGRAM_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Native rule polarity before parent-directory traversal is considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitignorePolarity {
    /// A matching rule excludes a path.
    Exclude,
    /// A matching negation permits a path when its ancestors permit traversal.
    Reinclude,
}

/// Native path scope after leading and terminal rule modifiers are removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitignoreAnchoring {
    /// A slash-free pattern may match a basename at any depth.
    AnyDepth,
    /// A leading or internal slash makes matching relative to the ignore root.
    RelativePath,
}

/// Source-derived metadata for one active rule; vector order is evaluation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitignoreRule {
    /// Effective raw line, excluding initial BOMs and its CRLF or LF terminator.
    pub line: SourceSpan,
    /// Exact pattern body after lexical modifiers; this range may be empty.
    pub pattern: SourceSpan,
    /// One-based physical source line, including preceding comments and blanks.
    pub line_number: u64,
    /// Native exclusion or reinclusion intent.
    pub polarity: GitignorePolarity,
    /// Whether a leading slash explicitly anchors the authored rule.
    pub leading_slash: bool,
    /// Whether the terminal slash restricts the rule to directories.
    pub directory_only: bool,
    /// Relative path or any-depth basename matching scope.
    pub anchoring: GitignoreAnchoring,
}

/// A bounded complete physical source and its exact ordered active-rule inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitignoreProgram {
    source: SourceRef,
    text: String,
    rules: Vec<GitignoreRule>,
}

/// Invalid source ownership, incomplete rule inventory, bounds or canonical encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid native gitignore program evidence")]
pub struct GitignoreProgramError;

impl GitignoreProgram {
    /// Authenticates every active source line and derives its lexical metadata.
    ///
    /// The supplied ranges must enumerate all non-comment, nonblank effective
    /// lines in source order. This validates ownership and lexical metadata,
    /// not glob grammar or the trust of the frontend's compilation attestation.
    ///
    /// # Errors
    /// Rejects incomplete or foreign ranges, a nonphysical source, hash mismatch,
    /// source/rule limits, allocation failure or inconsistent line ownership.
    pub fn from_evidence(
        source: SourceRef,
        text: &str,
        rule_lines: Vec<SourceSpan>,
    ) -> Result<Self, GitignoreProgramError> {
        if text.len() > MAX_GITIGNORE_SOURCE_BYTES
            || rule_lines.len() > MAX_GITIGNORE_RULES
            || source.span().start_byte() != 0
            || usize::try_from(source.span().end_byte()).ok() != Some(text.len())
            || source.content_hash() != content_hash(text.as_bytes())
        {
            return Err(GitignoreProgramError);
        }
        let mut rules = Vec::new();
        rules
            .try_reserve_exact(rule_lines.len())
            .map_err(|_| GitignoreProgramError)?;
        let mut offset = 0usize;
        for (index, physical) in text.split_inclusive('\n').enumerate() {
            let raw = physical.strip_suffix('\n').unwrap_or(physical);
            let raw = if physical.ends_with('\n') {
                raw.strip_suffix('\r').unwrap_or(raw)
            } else {
                raw
            };
            let effective = if index == 0 {
                raw.trim_start_matches('\u{feff}')
            } else {
                raw
            };
            let start = offset + raw.len() - effective.len();
            let line = span(source.span(), start, offset + raw.len())?;
            if let Some(rule) = lexical_rule(effective, line, index)? {
                if rules.len() >= MAX_GITIGNORE_RULES || rule_lines.get(rules.len()) != Some(&line)
                {
                    return Err(GitignoreProgramError);
                }
                rules.push(rule);
            }
            offset += physical.len();
        }
        if rules.len() != rule_lines.len() {
            return Err(GitignoreProgramError);
        }
        let mut owned = String::new();
        owned
            .try_reserve_exact(text.len())
            .map_err(|_| GitignoreProgramError)?;
        owned.push_str(text);
        Ok(Self {
            source,
            text: owned,
            rules,
        })
    }

    /// Returns the complete physical source identity shared by all rule ranges.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns the complete unchanged physical source, including comments and BOMs.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns active rules in evaluation order, retaining physical duplicates.
    #[must_use]
    pub fn rules(&self) -> &[GitignoreRule] {
        &self.rules
    }

    /// Verifies physical bytes independently of canonical envelope integrity.
    ///
    /// # Errors
    /// Rejects text differing from the admitted source file.
    pub fn verify_source_text(&self, text: &str) -> Result<(), GitignoreProgramError> {
        (text == self.text && content_hash(text.as_bytes()) == self.source.content_hash())
            .then_some(())
            .ok_or(GitignoreProgramError)
    }

    pub(crate) fn rebind(&self, generation: GenerationId) -> Result<Self, GitignoreProgramError> {
        let source = SourceRef::new(
            self.source.repository(),
            generation,
            self.source.span(),
            self.source.content_hash(),
            self.source.line_hint(),
        );
        Self::from_evidence(source, &self.text, rule_lines(&self.rules)?)
    }
}

fn span(owner: SourceSpan, start: usize, end: usize) -> Result<SourceSpan, GitignoreProgramError> {
    SourceSpan::new(
        owner.file(),
        start.try_into().map_err(|_| GitignoreProgramError)?,
        end.try_into().map_err(|_| GitignoreProgramError)?,
    )
    .map_err(|_| GitignoreProgramError)
}

fn lexical_rule(
    raw: &str,
    line: SourceSpan,
    index: usize,
) -> Result<Option<GitignoreRule>, GitignoreProgramError> {
    if raw.starts_with('#') {
        return Ok(None);
    }
    // Pinned ignore preserves the entire suffix when its final space is escaped.
    let trimmed = if raw.ends_with("\\ ") {
        raw
    } else {
        raw.trim_end()
    };
    if trimmed.is_empty() {
        return Ok(None);
    }
    let mut body = trimmed;
    let mut prefix = 0usize;
    let mut polarity = GitignorePolarity::Exclude;
    let mut leading_slash = false;
    if body.starts_with("\\!") || body.starts_with("\\#") {
        prefix += 1;
        body = body.get(1..).ok_or(GitignoreProgramError)?;
    } else {
        if let Some(rest) = body.strip_prefix('!') {
            polarity = GitignorePolarity::Reinclude;
            prefix += 1;
            body = rest;
        }
        if let Some(rest) = body.strip_prefix('/') {
            leading_slash = true;
            prefix += 1;
            body = rest;
        }
    }
    let directory_only = body.ends_with('/');
    if directory_only {
        body = body.strip_suffix('/').ok_or(GitignoreProgramError)?;
        body = body.strip_suffix('\\').unwrap_or(body);
    }
    let pattern_start =
        usize::try_from(line.start_byte()).map_err(|_| GitignoreProgramError)? + prefix;
    Ok(Some(GitignoreRule {
        line,
        pattern: span(line, pattern_start, pattern_start + body.len())?,
        line_number: u64::try_from(index).map_err(|_| GitignoreProgramError)? + 1,
        polarity,
        leading_slash,
        directory_only,
        anchoring: if leading_slash || body.contains('/') {
            GitignoreAnchoring::RelativePath
        } else {
            GitignoreAnchoring::AnyDepth
        },
    }))
}

fn rule_lines(rules: &[GitignoreRule]) -> Result<Vec<SourceSpan>, GitignoreProgramError> {
    if rules.len() > MAX_GITIGNORE_RULES {
        return Err(GitignoreProgramError);
    }
    let mut lines = Vec::new();
    lines
        .try_reserve_exact(rules.len())
        .map_err(|_| GitignoreProgramError)?;
    lines.extend(rules.iter().map(|rule| rule.line));
    Ok(lines)
}

/// Encodes a validated source-owned program with its canonical bounded identity.
///
/// # Errors
/// Rejects oversized canonical payloads, encoding or allocation failure.
pub fn new_gitignore_program_envelope(
    provenance: FactId,
    program: &GitignoreProgram,
) -> Result<ExtensionEnvelope, GitignoreProgramError> {
    let mut payload = Payload(Vec::new());
    serde_json::to_writer(&mut payload, program).map_err(|_| GitignoreProgramError)?;
    let payload = String::from_utf8(payload.0).map_err(|_| GitignoreProgramError)?;
    let source = program.source.clone();
    let derivation = vec![FactRef::File(source.span().file())];
    let mut identity = Payload(Vec::new());
    serde_json::to_writer(
        &mut identity,
        &(
            GITIGNORE_PROGRAM_NAMESPACE,
            GITIGNORE_PROGRAM_VERSION,
            &source,
            provenance,
            content_hash(payload.as_bytes()),
            &derivation,
        ),
    )
    .map_err(|_| GitignoreProgramError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.gitignore_program/v1", &identity.0).id(),
        repository: source.repository(),
        generation: source.generation(),
        namespace: GITIGNORE_PROGRAM_NAMESPACE.to_owned(),
        version: GITIGNORE_PROGRAM_VERSION.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation,
        },
    })
}

/// Reconstructs source metadata and verifies the complete canonical envelope.
///
/// # Errors
/// Rejects malformed/oversized payloads, incomplete inventories, forged metadata,
/// source ownership, envelope fields or noncanonical encoding.
pub fn decode_gitignore_program_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<GitignoreProgram, GitignoreProgramError> {
    if envelope.namespace != GITIGNORE_PROGRAM_NAMESPACE
        || envelope.payload.len() > MAX_GITIGNORE_PROGRAM_PAYLOAD_BYTES
    {
        return Err(GitignoreProgramError);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        source: SourceRef,
        text: String,
        rules: Vec<GitignoreRule>,
    }
    let wire: Wire = serde_json::from_str(&envelope.payload).map_err(|_| GitignoreProgramError)?;
    let program =
        GitignoreProgram::from_evidence(wire.source, &wire.text, rule_lines(&wire.rules)?)?;
    if wire.rules != program.rules
        || new_gitignore_program_envelope(envelope.provenance, &program)? != *envelope
    {
        return Err(GitignoreProgramError);
    }
    Ok(program)
}

struct Payload(Vec<u8>);
impl Write for Payload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > MAX_GITIGNORE_PROGRAM_PAYLOAD_BYTES)
        {
            return Err(io::Error::other("native gitignore payload limit"));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::other("native gitignore allocation"))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, RepositoryId};

    fn evidence(text: &str) -> (SourceRef, Vec<SourceSpan>) {
        let owner = SourceSpan::new(
            FileId::from_bytes([3; 20]),
            0,
            text.len().try_into().unwrap(),
        )
        .unwrap();
        let source = SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            owner,
            content_hash(text.as_bytes()),
            None,
        );
        let mut offset = 0;
        let mut lines = Vec::new();
        for (index, physical) in text.split_inclusive('\n').enumerate() {
            let raw = physical.strip_suffix('\n').unwrap_or(physical);
            let raw = if physical.ends_with('\n') {
                raw.strip_suffix('\r').unwrap_or(raw)
            } else {
                raw
            };
            let effective = if index == 0 {
                raw.trim_start_matches('\u{feff}')
            } else {
                raw
            };
            let line = span(
                owner,
                offset + raw.len() - effective.len(),
                offset + raw.len(),
            )
            .unwrap();
            if lexical_rule(effective, line, index).unwrap().is_some() {
                lines.push(line);
            }
            offset += physical.len();
        }
        (source, lines)
    }

    #[test]
    fn gitignore_metadata_preserves_bom_crlf_order_duplicates_and_native_modifiers() {
        let text = "\u{feff}\u{feff}# heading\r\n\r\n!/cache/  \r\nfoo/bar\n\\#literal\n\\!literal\nspace\\ \npath\\/\n[abc\n!\n/\nfoo/bar\n";
        let (source, lines) = evidence(text);
        let program = GitignoreProgram::from_evidence(source, text, lines).unwrap();
        assert_eq!(program.text(), text);
        assert_eq!(program.rules().len(), 10);
        let first = program.rules()[0];
        assert_eq!(first.line_number, 3);
        assert_eq!(first.polarity, GitignorePolarity::Reinclude);
        assert!(first.leading_slash && first.directory_only);
        assert_eq!(first.anchoring, GitignoreAnchoring::RelativePath);
        let patterns: Vec<_> = program
            .rules()
            .iter()
            .map(|rule| {
                text.get(
                    usize::try_from(rule.pattern.start_byte()).unwrap()
                        ..usize::try_from(rule.pattern.end_byte()).unwrap(),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(
            patterns,
            [
                "cache", "foo/bar", "#literal", "!literal", "space\\ ", "path", "[abc", "", "",
                "foo/bar"
            ]
        );
        assert!(!program.rules()[8].directory_only);
        assert_ne!(program.rules()[1].line, program.rules()[9].line);
        assert!(program.verify_source_text(text).is_ok());
        assert!(program.verify_source_text("changed").is_err());
        let (source, lines) = evidence("\u{feff}\u{feff}rule\r\n");
        let program =
            GitignoreProgram::from_evidence(source, "\u{feff}\u{feff}rule\r\n", lines).unwrap();
        assert_eq!(program.rules()[0].line.start_byte(), 6);
        assert_eq!(program.rules()[0].line.end_byte(), 10);
    }

    #[test]
    fn gitignore_inventory_rejects_omission_reordering_foreign_ranges_and_limits() {
        let (source, lines) = evidence("one\none\n");
        assert!(
            GitignoreProgram::from_evidence(source.clone(), "one\none\n", vec![lines[0]]).is_err()
        );
        assert!(
            GitignoreProgram::from_evidence(source.clone(), "one\none\n", vec![lines[1], lines[0]])
                .is_err()
        );
        let foreign = SourceSpan::new(FileId::from_bytes([9; 20]), 0, 3).unwrap();
        assert!(
            GitignoreProgram::from_evidence(source.clone(), "one\none\n", vec![foreign, lines[1]])
                .is_err()
        );
        assert!(GitignoreProgram::from_evidence(source, "different", lines).is_err());
        let empty = evidence("");
        assert!(GitignoreProgram::from_evidence(empty.0, "", empty.1).is_ok());
        let excessive = "a\n".repeat(MAX_GITIGNORE_RULES + 1);
        let (source, lines) = evidence(&excessive);
        assert!(GitignoreProgram::from_evidence(source, &excessive, lines).is_err());
        let large = "a".repeat(MAX_GITIGNORE_SOURCE_BYTES + 1);
        let (source, lines) = evidence(&large);
        assert!(GitignoreProgram::from_evidence(source, &large, lines).is_err());
    }

    #[test]
    fn gitignore_canonical_decoder_rejects_forged_metadata_and_envelope_fields() {
        let text = "!foo/\nfoo/\n";
        let (source, lines) = evidence(text);
        let program = GitignoreProgram::from_evidence(source, text, lines).unwrap();
        let envelope =
            new_gitignore_program_envelope(FactId::from_bytes([4; 20]), &program).unwrap();
        assert_eq!(
            decode_gitignore_program_envelope(&envelope).unwrap(),
            program
        );
        let mut changed = envelope.clone();
        let mut payload: serde_json::Value = serde_json::from_str(&changed.payload).unwrap();
        payload["rules"][0]["directory_only"] = false.into();
        changed.payload = serde_json::to_string(&payload).unwrap();
        assert!(decode_gitignore_program_envelope(&changed).is_err());
        changed = envelope.clone();
        changed.version = "2".into();
        assert!(decode_gitignore_program_envelope(&changed).is_err());
        changed = envelope.clone();
        changed.payload.push(' ');
        assert!(decode_gitignore_program_envelope(&changed).is_err());
        let rebound = program.rebind(GenerationId::from_bytes([5; 20])).unwrap();
        assert_ne!(rebound.source().generation(), program.source().generation());
        assert_eq!(rebound.rules(), program.rules());
        let successor = new_gitignore_program_envelope(envelope.provenance, &rebound).unwrap();
        assert_ne!(successor.id, envelope.id);
        assert_eq!(
            decode_gitignore_program_envelope(&successor).unwrap(),
            rebound
        );
    }
}
