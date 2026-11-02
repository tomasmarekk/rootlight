//! Retained C++ and declarative build bytes remain bound to the indexed generation.
//! Source authentication alone grants no include path or exact call authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::ControlFlow,
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    FileRecord, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef, SourceSpan,
};

mod calls;
mod cmake;
mod gradle;

pub(crate) struct WrittenCall {
    pub(crate) call: SourceRef,
    pub(crate) target: SymbolId,
    pub(crate) definition: SourceRef,
    pub(crate) definition_id: FactId,
    pub(crate) sources: Vec<SourceRef>,
    pub(crate) owners: Vec<SymbolId>,
}

#[derive(Default)]
pub(crate) struct CxxIndex {
    calls: BTreeMap<SourceSpan, Option<WrittenCall>>,
}

impl CxxIndex {
    pub(crate) fn proof(&self, occurrence: &OccurrenceRecord) -> Option<&WrittenCall> {
        if occurrence.role != OccurrenceRole::CallSite
            || occurrence.syntax_kind != "cpp.call.call"
            || matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
        {
            return None;
        }
        self.calls
            .get(&occurrence.source.span())
            .and_then(Option::as_ref)
            .filter(|proof| proof.call == occurrence.source)
    }

    pub(crate) fn resolve(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> Option<crate::ResolutionDecision> {
        let proof = self.proof(occurrence)?;
        let confidence =
            rootlight_ir::Confidence::new(900).expect("written binding confidence is valid");
        Some(crate::ResolutionDecision {
            occurrence: occurrence.id,
            outcome: crate::ResolutionOutcome::Resolved {
                symbol: proof.target,
                confidence,
            },
            explanation: crate::ResolutionExplanation {
                rule: crate::ResolutionRule::CppWrittenInclude,
                provider_name: crate::RESOLVER_PROVIDER_NAME,
                provider_version: crate::RESOLVER_PROVIDER_VERSION,
                candidates: vec![crate::CandidateExplanation {
                    symbol: proof.target,
                    score: confidence,
                    positive_signals: vec![crate::ResolutionSignal::NativeWrittenInclude],
                    penalties: Vec::new(),
                }],
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![
                    crate::CompletenessAssumption::ValidatedNormalizedDocument,
                    crate::CompletenessAssumption::SingleGeneration,
                    crate::CompletenessAssumption::SourceDeclaredCppIncludeSnapshot,
                    crate::CompletenessAssumption::NoRepositoryExecution,
                ],
            },
        })
    }
}

pub(super) struct CapturedSource<'document, 'bytes> {
    pub(super) file: &'document FileRecord,
    pub(super) bytes: &'bytes [u8],
}

pub(super) struct NativeBuildContext {
    pub(super) entry: String,
    pub(super) variables: BTreeMap<String, Option<String>>,
    pub(super) evidence: Vec<SourceRef>,
}

pub(super) fn full_source(file: &FileRecord) -> Result<SourceRef, ResolutionError> {
    file.evidence
        .source
        .clone()
        .filter(|source| {
            source.span().file() == file.id
                && source.content_hash() == file.content_hash
                && source.repository() == file.repository
                && source.generation() == file.generation
                && source.span().start_byte() == 0
                && source.span().end_byte() == file.byte_length
        })
        .ok_or(ResolutionError::CxxSourceIdentity)
}

pub(super) fn join(base: &str, relative: &str) -> Option<String> {
    if relative.starts_with('/') || relative.contains(['\\', ':', '\0']) {
        return None;
    }
    let mut parts: Vec<_> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

pub(super) fn parse(
    bytes: &[u8],
    language: tree_sitter::Language,
    work: &mut ResolutionWorkBudget,
    cancellation: &Cancellation,
) -> Result<Option<tree_sitter::Tree>, ResolutionError> {
    if bytes.len() > 1024 * 1024 || bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
        return Ok(None);
    }
    cancellation.check()?;
    let maximum = work
        .remaining()
        .checked_add(work.consumed())
        .ok_or(ResolutionError::CountOverflow)?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|_| ResolutionError::InvalidScore)?;
    let mut refused = false;
    let tree = {
        let mut progress = |_: &tree_sitter::ParseState| {
            if cancellation.check().is_err() {
                return ControlFlow::Break(());
            }
            if work.consume().is_err() {
                refused = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        };
        let mut input = |offset: usize, _: tree_sitter::Point| {
            bytes
                .get(offset..offset.saturating_add(4096).min(bytes.len()))
                .unwrap_or_default()
        };
        parser.parse_with_options(
            &mut input,
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        )
    };
    cancellation.check()?;
    if refused {
        return Err(ResolutionError::WorkLimit { maximum });
    }
    Ok(tree.filter(|tree| tree.root_node().end_byte() == bytes.len()))
}

use crate::{
    ResolutionError,
    engine::{CandidateIndex, ResolutionWorkBudget},
};

/// Immutable bytes for an indexed C++ source or declarative build input.
///
/// The resolver authenticates their complete identity before interpreting any
/// written include authority. This input does not assert a configured compiler profile.
#[derive(Debug, Clone, Copy)]
pub struct CxxContextInput<'a> {
    file: FileId,
    bytes: &'a [u8],
}

impl<'a> CxxContextInput<'a> {
    /// Associates captured bytes with a file; resolution verifies hash and length.
    #[must_use]
    pub const fn new(file: FileId, bytes: &'a [u8]) -> Self {
        Self { file, bytes }
    }

    /// Selects source categories that may contribute written C++ build context.
    /// Selection is not evidence of an include root or of project membership.
    #[must_use]
    pub fn accepts(file: &FileRecord) -> bool {
        file.language == "cpp"
            || file.path.rsplit('/').next().is_some_and(|name| {
                name == "CMakeLists.txt"
                    || name == "compile_commands.json"
                    || name.ends_with(".cmake")
                    || name.ends_with(".gradle")
                    || name.ends_with(".gradle.kts")
            })
    }
}

pub(crate) fn build(
    index: &CandidateIndex<'_>,
    occurrences: &[OccurrenceRecord],
    inputs: &[CxxContextInput<'_>],
    work: &mut ResolutionWorkBudget,
    cancellation: &Cancellation,
) -> Result<CxxIndex, ResolutionError> {
    if inputs.is_empty() {
        return Ok(CxxIndex::default());
    }
    let mut seen = BTreeSet::new();
    let mut captured = BTreeMap::new();
    for input in inputs {
        cancellation.check()?;
        work.consume()?;
        let file = index
            .files
            .get(&input.file)
            .ok_or(ResolutionError::CxxSourceIdentity)?;
        if !CxxContextInput::accepts(file)
            || !seen.insert(input.file)
            || u64::try_from(input.bytes.len()).ok() != Some(file.byte_length)
        {
            return Err(ResolutionError::CxxSourceIdentity);
        }
        for _ in input.bytes.chunks(4096) {
            cancellation.check()?;
            work.consume()?;
        }
        if content_hash(input.bytes) != file.content_hash {
            return Err(ResolutionError::CxxSourceIdentity);
        }
        captured.insert(
            input.file,
            CapturedSource {
                file,
                bytes: input.bytes,
            },
        );
    }
    let contexts = gradle::builds(&captured, work, cancellation)?;
    let includes = cmake::includes(&captured, contexts, work, cancellation)?;
    if includes.is_empty() {
        return Ok(CxxIndex::default());
    }
    calls::build(index, occurrences, &captured, &includes, work, cancellation)
}
