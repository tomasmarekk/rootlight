//! Ordered CODEOWNERS intent over one complete immutable source file.
//! Independent lexical reconstruction authenticates valid and invalid lines;
//! repository matching and account authority remain outside this contract.

use std::io::{self, Write};

use rootlight_ids::{FactId, GenerationId, content_hash, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for ordered source-backed CODEOWNERS programs.
pub const CODEOWNERS_PROGRAM_NAMESPACE: &str = "rootlight.codeowners_program";
/// Canonical CODEOWNERS program payload revision.
pub const CODEOWNERS_PROGRAM_VERSION: &str = "1";
/// Maximum complete physical source size in bytes.
pub const MAX_CODEOWNERS_SOURCE_BYTES: usize = 1024 * 1024;
/// Maximum combined valid and invalid active lines, including duplicates.
pub const MAX_CODEOWNERS_ACTIVE_LINES: usize = 4096;
/// Maximum canonical payload size, including JSON escape expansion.
pub const MAX_CODEOWNERS_PROGRAM_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Authored owner-token shape, without external account or permission proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeownersOwnerKind {
    /// A nonempty `@name` token.
    User,
    /// A nonempty `@organization/team` token.
    Team,
    /// A nonleading single `@` separating two nonempty token components.
    Email,
}

/// One unchanged owner token and its independently derived lexical shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeownersOwner {
    /// Exact physical owner-token bytes, including an authored leading `@`.
    pub span: SourceSpan,
    /// Token shape only; no account existence or repository authority is implied.
    pub kind: CodeownersOwnerKind,
}

/// A complete authored rule; vector order is evaluation order and repeats remain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeownersRule {
    /// Effective physical line, excluding initial BOMs and its CRLF/LF terminator.
    pub line: SourceSpan,
    /// Exact raw pattern token, preserving escape bytes.
    pub pattern: SourceSpan,
    /// One-based physical line number, including comments and blank lines.
    pub line_number: u64,
    /// Authored owner tokens; an empty vector is a valid ownership-clearing rule.
    pub owners: Vec<CodeownersOwner>,
}

/// A source-derived lexical reason that invalidates the entire active line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeownersInvalidReason {
    /// An unescaped leading `!` attempts unsupported pattern negation.
    UnsupportedNegation,
    /// An unescaped bracket attempts unsupported character-class syntax.
    UnsupportedCharacterClass,
    /// A leading `\#` attempts an unsupported escaped comment marker.
    UnsupportedEscapedComment,
    /// A raw pattern ends before its final escape has a following character.
    DanglingEscape,
    /// A token contains a control character other than a whitespace separator.
    ControlCharacter,
    /// At least one owner token lacks a supported lexical shape.
    InvalidOwner,
}

impl CodeownersInvalidReason {
    /// Returns the stable diagnostic code for this authenticated lexical reason.
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::UnsupportedNegation => "codeowners-negation-unsupported",
            Self::UnsupportedCharacterClass => "codeowners-character-class-unsupported",
            Self::UnsupportedEscapedComment => "codeowners-escaped-comment-unsupported",
            Self::DanglingEscape => "codeowners-dangling-pattern-escape",
            Self::ControlCharacter => "codeowners-control-character",
            Self::InvalidOwner => "codeowners-owner-syntax-invalid",
        }
    }
}

/// One invalid active line, retained in physical source order with its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeownersInvalidLine {
    /// Complete effective physical line, including any inline comment.
    pub line: SourceSpan,
    /// One-based physical line number.
    pub line_number: u64,
    /// Independently derived reason; compiler-only failures cannot be attested here.
    pub reason: CodeownersInvalidReason,
}

/// A complete physical source and its exact valid/invalid active-line inventories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeownersProgram {
    source: SourceRef,
    text: String,
    rules: Vec<CodeownersRule>,
    invalid_lines: Vec<CodeownersInvalidLine>,
}

/// Invalid source ownership, inventory, bounds, allocation or canonical encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid native codeowners program evidence")]
pub struct CodeownersProgramError;

impl CodeownersProgram {
    /// Derives every active line from complete authenticated physical UTF-8 text.
    ///
    /// Valid and invalid lines remain separate ordered inventories. Owner tokens
    /// express authored intent; glob compilation and account authority are not proved.
    ///
    /// # Errors
    /// Rejects nonphysical or hash-mismatched source, source/line bounds, allocation
    /// failure or inconsistent physical byte ranges.
    pub fn from_source(source: SourceRef, text: &str) -> Result<Self, CodeownersProgramError> {
        if text.len() > MAX_CODEOWNERS_SOURCE_BYTES
            || source.span().start_byte() != 0
            || usize::try_from(source.span().end_byte()).ok() != Some(text.len())
            || source.content_hash() != content_hash(text.as_bytes())
        {
            return Err(CodeownersProgramError);
        }
        let mut rules = Vec::new();
        let mut invalid_lines = Vec::new();
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
            let start = offset
                .checked_add(raw.len() - effective.len())
                .ok_or(CodeownersProgramError)?;
            let end = offset
                .checked_add(raw.len())
                .ok_or(CodeownersProgramError)?;
            let line = physical_span(source.span(), start, end)?;
            let line_number = u64::try_from(index)
                .map_err(|_| CodeownersProgramError)?
                .checked_add(1)
                .ok_or(CodeownersProgramError)?;
            let parsed = classify_line(effective, line, line_number)?;
            if !matches!(parsed, ParsedLine::Ignored) {
                let active = rules
                    .len()
                    .checked_add(invalid_lines.len())
                    .ok_or(CodeownersProgramError)?;
                if active >= MAX_CODEOWNERS_ACTIVE_LINES {
                    return Err(CodeownersProgramError);
                }
            }
            match parsed {
                ParsedLine::Ignored => {}
                ParsedLine::Rule(rule) => {
                    rules.try_reserve(1).map_err(|_| CodeownersProgramError)?;
                    rules.push(rule);
                }
                ParsedLine::Invalid(reason) => {
                    invalid_lines
                        .try_reserve(1)
                        .map_err(|_| CodeownersProgramError)?;
                    invalid_lines.push(CodeownersInvalidLine {
                        line,
                        line_number,
                        reason,
                    });
                }
            }
            offset = offset
                .checked_add(physical.len())
                .ok_or(CodeownersProgramError)?;
        }
        let mut owned = String::new();
        owned
            .try_reserve_exact(text.len())
            .map_err(|_| CodeownersProgramError)?;
        owned.push_str(text);
        Ok(Self {
            source,
            text: owned,
            rules,
            invalid_lines,
        })
    }

    /// Authenticates complete separately ordered valid and invalid line evidence.
    ///
    /// Every active line is reclassified independently from physical text. Supplied
    /// invalid reasons cannot attest a frontend compiler failure.
    ///
    /// # Errors
    /// Rejects the errors of [`Self::from_source`], omissions, duplicates, reordered
    /// or foreign spans, forged invalid reasons or combined inventory overflow.
    pub fn from_evidence(
        source: SourceRef,
        text: &str,
        rule_lines: Vec<SourceSpan>,
        invalid_lines: Vec<CodeownersInvalidLine>,
    ) -> Result<Self, CodeownersProgramError> {
        if rule_lines
            .len()
            .checked_add(invalid_lines.len())
            .is_none_or(|count| count > MAX_CODEOWNERS_ACTIVE_LINES)
        {
            return Err(CodeownersProgramError);
        }
        let program = Self::from_source(source, text)?;
        if !program.rules.iter().map(|rule| rule.line).eq(rule_lines)
            || program.invalid_lines != invalid_lines
        {
            return Err(CodeownersProgramError);
        }
        Ok(program)
    }

    /// Returns the complete physical source identity shared by every range.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns unchanged physical source bytes as UTF-8, including BOMs/comments.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns valid rules in source order, preserving duplicates and clearing rules.
    #[must_use]
    pub fn rules(&self) -> &[CodeownersRule] {
        &self.rules
    }

    /// Returns all invalid active lines in source order.
    #[must_use]
    pub fn invalid_lines(&self) -> &[CodeownersInvalidLine] {
        &self.invalid_lines
    }

    /// Verifies complete physical bytes independently of canonical envelope integrity.
    ///
    /// # Errors
    /// Rejects text differing from the authenticated physical source.
    pub fn verify_source_text(&self, text: &str) -> Result<(), CodeownersProgramError> {
        (text == self.text && content_hash(text.as_bytes()) == self.source.content_hash())
            .then_some(())
            .ok_or(CodeownersProgramError)
    }

    pub(crate) fn rebind(&self, generation: GenerationId) -> Result<Self, CodeownersProgramError> {
        let source = SourceRef::new(
            self.source.repository(),
            generation,
            self.source.span(),
            self.source.content_hash(),
            self.source.line_hint(),
        );
        Self::from_source(source, &self.text)
    }
}

enum ParsedLine {
    Ignored,
    Rule(CodeownersRule),
    Invalid(CodeownersInvalidReason),
}

fn classify_line(
    raw: &str,
    line: SourceSpan,
    line_number: u64,
) -> Result<ParsedLine, CodeownersProgramError> {
    let content = raw.trim_start_matches(|character: char| character.is_ascii_whitespace());
    if content.is_empty() || content.starts_with('#') {
        return Ok(ParsedLine::Ignored);
    }
    if content.starts_with('!') {
        return Ok(ParsedLine::Invalid(
            CodeownersInvalidReason::UnsupportedNegation,
        ));
    }
    if content.starts_with("\\#") {
        return Ok(ParsedLine::Invalid(
            CodeownersInvalidReason::UnsupportedEscapedComment,
        ));
    }
    let start = raw.len() - content.len();
    let mut end = raw.len();
    let mut characters = content.char_indices();
    while let Some((relative, character)) = characters.next() {
        if character.is_ascii_whitespace() {
            end = start.checked_add(relative).ok_or(CodeownersProgramError)?;
            break;
        }
        if character.is_control() {
            return Ok(ParsedLine::Invalid(
                CodeownersInvalidReason::ControlCharacter,
            ));
        }
        if character == '\\' {
            let Some((_, escaped)) = characters.next() else {
                return Ok(ParsedLine::Invalid(CodeownersInvalidReason::DanglingEscape));
            };
            if escaped.is_control() {
                return Ok(ParsedLine::Invalid(
                    CodeownersInvalidReason::ControlCharacter,
                ));
            }
        } else if matches!(character, '[' | ']') {
            return Ok(ParsedLine::Invalid(
                CodeownersInvalidReason::UnsupportedCharacterClass,
            ));
        }
    }
    let mut owners = Vec::new();
    let mut cursor = end;
    loop {
        let remaining = raw.get(cursor..).ok_or(CodeownersProgramError)?;
        let token_source =
            remaining.trim_start_matches(|character: char| character.is_ascii_whitespace());
        cursor = cursor
            .checked_add(remaining.len() - token_source.len())
            .ok_or(CodeownersProgramError)?;
        if token_source.is_empty() || token_source.starts_with('#') {
            break;
        }
        let token_len = token_source
            .find(|character: char| character.is_ascii_whitespace())
            .unwrap_or(token_source.len());
        let token = token_source
            .get(..token_len)
            .ok_or(CodeownersProgramError)?;
        if token.chars().any(char::is_control) {
            return Ok(ParsedLine::Invalid(
                CodeownersInvalidReason::ControlCharacter,
            ));
        }
        let Some(kind) = owner_kind(token) else {
            return Ok(ParsedLine::Invalid(CodeownersInvalidReason::InvalidOwner));
        };
        let next = cursor
            .checked_add(token_len)
            .ok_or(CodeownersProgramError)?;
        owners.try_reserve(1).map_err(|_| CodeownersProgramError)?;
        owners.push(CodeownersOwner {
            span: relative_span(line, cursor, next)?,
            kind,
        });
        cursor = next;
    }
    Ok(ParsedLine::Rule(CodeownersRule {
        line,
        pattern: relative_span(line, start, end)?,
        line_number,
        owners,
    }))
}

fn owner_kind(token: &str) -> Option<CodeownersOwnerKind> {
    if token.chars().any(|character| {
        character.is_control() || character.is_ascii_whitespace() || matches!(character, '\\' | '#')
    }) {
        return None;
    }
    if let Some(name) = token.strip_prefix('@') {
        if name.is_empty() || name.contains('@') {
            return None;
        }
        if let Some((organization, team)) = name.split_once('/') {
            return (!organization.is_empty() && !team.is_empty() && !team.contains('/'))
                .then_some(CodeownersOwnerKind::Team);
        }
        return Some(CodeownersOwnerKind::User);
    }
    let (local, domain) = token.split_once('@')?;
    (!local.is_empty() && !domain.is_empty() && !domain.contains('@') && !token.contains('/'))
        .then_some(CodeownersOwnerKind::Email)
}

fn relative_span(
    line: SourceSpan,
    start: usize,
    end: usize,
) -> Result<SourceSpan, CodeownersProgramError> {
    let offset = usize::try_from(line.start_byte()).map_err(|_| CodeownersProgramError)?;
    physical_span(
        line,
        offset.checked_add(start).ok_or(CodeownersProgramError)?,
        offset.checked_add(end).ok_or(CodeownersProgramError)?,
    )
}

fn physical_span(
    owner: SourceSpan,
    start: usize,
    end: usize,
) -> Result<SourceSpan, CodeownersProgramError> {
    SourceSpan::new(
        owner.file(),
        start.try_into().map_err(|_| CodeownersProgramError)?,
        end.try_into().map_err(|_| CodeownersProgramError)?,
    )
    .map_err(|_| CodeownersProgramError)
}

/// Encodes an authenticated program with its canonical bounded source identity.
///
/// # Errors
/// Rejects oversized canonical payloads, serialization or allocation failure.
pub fn new_codeowners_program_envelope(
    provenance: FactId,
    program: &CodeownersProgram,
) -> Result<ExtensionEnvelope, CodeownersProgramError> {
    let mut payload = Payload(Vec::new());
    serde_json::to_writer(&mut payload, program).map_err(|_| CodeownersProgramError)?;
    let payload = String::from_utf8(payload.0).map_err(|_| CodeownersProgramError)?;
    let source = program.source.clone();
    let derivation = vec![FactRef::File(source.span().file())];
    let mut identity = Payload(Vec::new());
    serde_json::to_writer(
        &mut identity,
        &(
            CODEOWNERS_PROGRAM_NAMESPACE,
            CODEOWNERS_PROGRAM_VERSION,
            &source,
            provenance,
            content_hash(payload.as_bytes()),
            &derivation,
        ),
    )
    .map_err(|_| CodeownersProgramError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.codeowners_program/v1", &identity.0).id(),
        repository: source.repository(),
        generation: source.generation(),
        namespace: CODEOWNERS_PROGRAM_NAMESPACE.to_owned(),
        version: CODEOWNERS_PROGRAM_VERSION.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation,
        },
    })
}

/// Independently reconstructs all source inventories and validates the full envelope.
///
/// # Errors
/// Rejects malformed or oversized payloads, forged source/metadata, incomplete or
/// reordered inventories, incompatible fields and noncanonical encoding.
pub fn decode_codeowners_program_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<CodeownersProgram, CodeownersProgramError> {
    if envelope.namespace != CODEOWNERS_PROGRAM_NAMESPACE
        || envelope.payload.len() > MAX_CODEOWNERS_PROGRAM_PAYLOAD_BYTES
    {
        return Err(CodeownersProgramError);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        source: SourceRef,
        text: String,
        rules: Vec<CodeownersRule>,
        invalid_lines: Vec<CodeownersInvalidLine>,
    }
    let wire: Wire = serde_json::from_str(&envelope.payload).map_err(|_| CodeownersProgramError)?;
    let mut lines = Vec::new();
    lines
        .try_reserve_exact(wire.rules.len())
        .map_err(|_| CodeownersProgramError)?;
    lines.extend(wire.rules.iter().map(|rule| rule.line));
    let program =
        CodeownersProgram::from_evidence(wire.source, &wire.text, lines, wire.invalid_lines)?;
    if wire.rules != program.rules
        || new_codeowners_program_envelope(envelope.provenance, &program)? != *envelope
    {
        return Err(CodeownersProgramError);
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
            .is_none_or(|size| size > MAX_CODEOWNERS_PROGRAM_PAYLOAD_BYTES)
        {
            return Err(io::Error::other("native codeowners payload limit"));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::other("native codeowners allocation"))?;
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

    fn source(text: &str) -> SourceRef {
        SourceRef::new(
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
        )
    }

    fn text_at(text: &str, span: SourceSpan) -> &str {
        &text
            [usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()]
    }

    #[test]
    fn codeowners_metadata_preserves_physical_order_owner_shapes_and_clearing_rules() {
        let text = "\u{feff}# source\r\nsrc/** @Owner @Org/Team docs@example.com # note\r\nsrc/** @Owner\r\nclear/\r\nfoo\\ bar @user\r\nfoo#bar @user\r\n\\!literal @user";
        let program = CodeownersProgram::from_source(source(text), text).unwrap();
        assert_eq!(program.rules().len(), 6);
        assert!(program.invalid_lines().is_empty());
        assert_eq!(
            program
                .rules()
                .iter()
                .map(|rule| text_at(text, rule.pattern))
                .collect::<Vec<_>>(),
            [
                "src/**",
                "src/**",
                "clear/",
                "foo\\ bar",
                "foo#bar",
                "\\!literal"
            ]
        );
        let first = &program.rules()[0];
        assert_eq!(first.line_number, 2);
        assert_eq!(
            first
                .owners
                .iter()
                .map(|owner| owner.kind)
                .collect::<Vec<_>>(),
            [
                CodeownersOwnerKind::User,
                CodeownersOwnerKind::Team,
                CodeownersOwnerKind::Email
            ]
        );
        assert_eq!(
            first
                .owners
                .iter()
                .map(|owner| text_at(text, owner.span))
                .collect::<Vec<_>>(),
            ["@Owner", "@Org/Team", "docs@example.com"]
        );
        assert!(program.rules()[2].owners.is_empty());
        assert_ne!(program.rules()[0].line, program.rules()[1].line);
        assert!(program.verify_source_text(text).is_ok());
        assert!(program.verify_source_text("changed").is_err());
        let bom = "\u{feff}\u{feff}a @user\r\n";
        let program = CodeownersProgram::from_source(source(bom), bom).unwrap();
        assert_eq!(program.rules()[0].line.start_byte(), 6);
        assert_eq!(text_at(bom, program.rules()[0].line), "a @user");
    }

    #[test]
    fn codeowners_invalid_lines_are_complete_typed_and_never_partial_rules() {
        let text =
            "!bad @user\n[ab] @user\n\\#bad @user\nbad\\\npa\u{1}th @user\npath @valid malformed\n";
        let program = CodeownersProgram::from_source(source(text), text).unwrap();
        assert!(program.rules().is_empty());
        assert_eq!(
            program
                .invalid_lines()
                .iter()
                .map(|line| line.reason)
                .collect::<Vec<_>>(),
            [
                CodeownersInvalidReason::UnsupportedNegation,
                CodeownersInvalidReason::UnsupportedCharacterClass,
                CodeownersInvalidReason::UnsupportedEscapedComment,
                CodeownersInvalidReason::DanglingEscape,
                CodeownersInvalidReason::ControlCharacter,
                CodeownersInvalidReason::InvalidOwner,
            ]
        );
        for (index, invalid) in program.invalid_lines().iter().enumerate() {
            assert_eq!(invalid.line_number, u64::try_from(index).unwrap() + 1);
            assert_eq!(
                text_at(text, invalid.line),
                text.lines().nth(index).unwrap()
            );
            assert!(invalid.reason.diagnostic_code().starts_with("codeowners-"));
        }
        for owner in [
            "@",
            "@org/",
            "@/team",
            "@org/team/extra",
            "@two@names",
            "local@@host",
            "local@",
            "local@host/path",
            "@user\\name",
            "@user#note",
        ] {
            let text = format!("path @valid {owner}\n");
            let program = CodeownersProgram::from_source(source(&text), &text).unwrap();
            assert!(program.rules().is_empty(), "{owner}");
            assert_eq!(program.invalid_lines().len(), 1, "{owner}");
        }
    }

    #[test]
    fn codeowners_inventory_rejects_omitted_reordered_foreign_and_forged_evidence() {
        let text = "one @user\n!bad @user\none @user\n!bad @user\n";
        let program = CodeownersProgram::from_source(source(text), text).unwrap();
        let lines = program
            .rules()
            .iter()
            .map(|rule| rule.line)
            .collect::<Vec<_>>();
        let invalid = program.invalid_lines().to_vec();
        assert_eq!(
            CodeownersProgram::from_evidence(source(text), text, lines.clone(), invalid.clone())
                .unwrap(),
            program
        );
        assert!(
            CodeownersProgram::from_evidence(source(text), text, vec![lines[0]], invalid.clone())
                .is_err()
        );
        assert!(
            CodeownersProgram::from_evidence(
                source(text),
                text,
                vec![lines[1], lines[0]],
                invalid.clone()
            )
            .is_err()
        );
        assert!(
            CodeownersProgram::from_evidence(source(text), text, lines.clone(), vec![invalid[0]])
                .is_err()
        );
        assert!(
            CodeownersProgram::from_evidence(
                source(text),
                text,
                lines.clone(),
                vec![invalid[1], invalid[0]]
            )
            .is_err()
        );
        let foreign = SourceSpan::new(FileId::from_bytes([9; 20]), 0, 9).unwrap();
        assert!(
            CodeownersProgram::from_evidence(
                source(text),
                text,
                vec![foreign, lines[1]],
                invalid.clone()
            )
            .is_err()
        );
        let mut forged = invalid;
        forged[0].reason = CodeownersInvalidReason::InvalidOwner;
        assert!(CodeownersProgram::from_evidence(source(text), text, lines, forged).is_err());
        assert!(CodeownersProgram::from_source(source(text), "different").is_err());
        let excessive = format!(
            "{}a @u\n",
            "a @u\n!bad\n".repeat(MAX_CODEOWNERS_ACTIVE_LINES / 2)
        );
        assert!(CodeownersProgram::from_source(source(&excessive), &excessive).is_err());
        let oversized = "a".repeat(MAX_CODEOWNERS_SOURCE_BYTES + 1);
        assert!(CodeownersProgram::from_source(source(&oversized), &oversized).is_err());
        assert!(CodeownersProgram::from_source(source(""), "").is_ok());
    }

    #[test]
    fn codeowners_canonical_decoder_reconstructs_every_rule_and_invalid_reason() {
        let text = "path @owner @org/team\n!bad @user\nclear/\n";
        let program = CodeownersProgram::from_source(source(text), text).unwrap();
        let envelope =
            new_codeowners_program_envelope(FactId::from_bytes([4; 20]), &program).unwrap();
        assert_eq!(
            decode_codeowners_program_envelope(&envelope).unwrap(),
            program
        );
        for field in ["owners", "pattern", "invalid", "omission"] {
            let mut changed = envelope.clone();
            let mut wire: serde_json::Value = serde_json::from_str(&changed.payload).unwrap();
            match field {
                "owners" => wire["rules"][0]["owners"][0]["kind"] = "email".into(),
                "pattern" => wire["rules"][0]["pattern"]["start_byte"] = 1.into(),
                "invalid" => wire["invalid_lines"][0]["reason"] = "invalid_owner".into(),
                "omission" => {
                    wire["rules"].as_array_mut().unwrap().pop();
                }
                _ => unreachable!(),
            }
            changed.payload = serde_json::to_string(&wire).unwrap();
            assert!(
                decode_codeowners_program_envelope(&changed).is_err(),
                "{field}"
            );
        }
        let mut changed = envelope.clone();
        changed.payload.push(' ');
        assert!(decode_codeowners_program_envelope(&changed).is_err());
        changed = envelope.clone();
        changed.version = "2".into();
        assert!(decode_codeowners_program_envelope(&changed).is_err());
        changed = envelope.clone();
        changed.evidence.source = None;
        assert!(decode_codeowners_program_envelope(&changed).is_err());
        let rebound = program.rebind(GenerationId::from_bytes([5; 20])).unwrap();
        assert_eq!(rebound.rules(), program.rules());
        assert_eq!(rebound.invalid_lines(), program.invalid_lines());
        let successor = new_codeowners_program_envelope(envelope.provenance, &rebound).unwrap();
        assert_ne!(successor.id, envelope.id);
        assert_eq!(
            decode_codeowners_program_envelope(&successor).unwrap(),
            rebound
        );
    }
}
