//! Deterministic source-bound semantic fallback evidence generator.
//!
//! The fixed contract corpus measures ambiguity handling but never authorizes
//! language-tier promotion or production semantic acceptance.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    io::{self, Write as _},
    process::ExitCode,
};

use rootlight_adapters::initial_semantic_registry;
use rootlight_bench::{build_semantic_evidence, encode_semantic_evidence_envelope};
use rootlight_ids::{FactId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{Confidence, CoverageStatus};
use rootlight_resolve::{
    ExpectedResolution, RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionBatch,
    ResolutionDecision, ResolutionExpectation, ResolutionExplanation, ResolutionOutcome,
    ResolutionRule, UnresolvedReason,
};

const MAX_ARGUMENT_BYTES: usize = 16 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(encoded) => {
            let mut stdout = io::stdout().lock();
            if stdout
                .write_all(&encoded)
                .and_then(|()| stdout.write_all(b"\n"))
                .is_ok()
            {
                ExitCode::SUCCESS
            } else {
                eprintln!("error: semantic evidence could not be written");
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Vec<u8>, &'static str> {
    let source_revision = parse_arguments(std::env::args_os().skip(1))?;
    let registry =
        initial_semantic_registry().map_err(|_| "semantic language registry is invalid")?;
    let (batch, expectations) = contract_fixture()?;
    let evidence = build_semantic_evidence(&registry, &batch, &expectations)
        .map_err(|_| "semantic contract evidence is invalid")?;
    encode_semantic_evidence_envelope(&evidence, &source_revision)
        .map_err(|_| "semantic evidence envelope is invalid")
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<String, &'static str> {
    let mut arguments = arguments.into_iter();
    let flag = next_argument(&mut arguments)?.ok_or("semantic evidence arguments are invalid")?;
    if flag != OsStr::new("--source-revision") {
        return Err("semantic evidence arguments are invalid");
    }
    let source_revision = next_argument(&mut arguments)?
        .and_then(|value| value.into_string().ok())
        .ok_or("semantic evidence arguments are invalid")?;
    if next_argument(&mut arguments)?.is_some() {
        return Err("semantic evidence arguments are invalid");
    }
    Ok(source_revision)
}

fn next_argument<I>(arguments: &mut I) -> Result<Option<OsString>, &'static str>
where
    I: Iterator<Item = OsString>,
{
    let Some(argument) = arguments.next() else {
        return Ok(None);
    };
    if argument.as_encoded_bytes().len() > MAX_ARGUMENT_BYTES {
        return Err("semantic evidence arguments are invalid");
    }
    Ok(Some(argument))
}

fn contract_fixture() -> Result<(ResolutionBatch, Vec<ResolutionExpectation>), &'static str> {
    let exact_occurrence = FactId::from_bytes([1; 20]);
    let ambiguous_occurrence = FactId::from_bytes([2; 20]);
    let unresolved_occurrence = FactId::from_bytes([3; 20]);
    let exact_symbol = SymbolId::from_bytes([11; 20]);
    let candidate_symbol = SymbolId::from_bytes([12; 20]);
    let exact_confidence =
        Confidence::new(1_000).map_err(|_| "semantic fixture confidence is invalid")?;
    let candidate_confidence =
        Confidence::new(900).map_err(|_| "semantic fixture confidence is invalid")?;
    let unresolved_confidence =
        Confidence::new(0).map_err(|_| "semantic fixture confidence is invalid")?;
    let decisions = vec![
        decision(
            exact_occurrence,
            ResolutionOutcome::Resolved {
                symbol: exact_symbol,
                confidence: exact_confidence,
            },
        ),
        decision(
            ambiguous_occurrence,
            ResolutionOutcome::Candidates {
                symbols: vec![candidate_symbol, SymbolId::from_bytes([13; 20])],
                total_count: 2,
                completeness: CoverageStatus::Complete,
                confidence: candidate_confidence,
            },
        ),
        decision(
            unresolved_occurrence,
            ResolutionOutcome::Unresolved {
                reason: UnresolvedReason::NoCandidate,
                confidence: unresolved_confidence,
            },
        ),
    ];
    let expectations = vec![
        ResolutionExpectation {
            occurrence: exact_occurrence,
            expected: ExpectedResolution::Exact(exact_symbol),
        },
        ResolutionExpectation {
            occurrence: ambiguous_occurrence,
            expected: ExpectedResolution::CandidateContains(candidate_symbol),
        },
        ResolutionExpectation {
            occurrence: unresolved_occurrence,
            expected: ExpectedResolution::Unresolved,
        },
    ];
    Ok((
        ResolutionBatch {
            repository: RepositoryId::from_bytes([21; 16]),
            generation: GenerationId::from_bytes([22; 20]),
            decisions,
        },
        expectations,
    ))
}

fn decision(occurrence: FactId, outcome: ResolutionOutcome) -> ResolutionDecision {
    ResolutionDecision {
        occurrence,
        outcome,
        explanation: ResolutionExplanation {
            rule: ResolutionRule::LexicalScope,
            provider_name: RESOLVER_PROVIDER_NAME,
            provider_version: RESOLVER_PROVIDER_VERSION,
            candidates: Vec::new(),
            rejected_candidates: Vec::new(),
            rejected_total: 0,
            completeness_assumptions: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_reject_missing_extra_and_oversized_values() {
        let revision = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            parse_arguments(
                ["--source-revision", revision]
                    .into_iter()
                    .map(OsString::from)
            )
            .expect("canonical arguments are accepted"),
            revision
        );
        assert!(parse_arguments(std::iter::empty()).is_err());
        assert!(
            parse_arguments(
                ["--source-revision", revision, "--extra"]
                    .into_iter()
                    .map(OsString::from)
            )
            .is_err()
        );
        assert!(
            parse_arguments([
                OsString::from("--source-revision"),
                OsString::from("x".repeat(MAX_ARGUMENT_BYTES + 1)),
            ])
            .is_err()
        );
    }

    #[test]
    fn fixed_fixture_cannot_authorize_production_acceptance() {
        let registry = initial_semantic_registry().expect("shared registry is valid");
        let (batch, expectations) = contract_fixture().expect("contract fixture is valid");
        let evidence = build_semantic_evidence(&registry, &batch, &expectations)
            .expect("contract fixture evidence builds");
        let encoded = encode_semantic_evidence_envelope(
            &evidence,
            "0123456789abcdef0123456789abcdef01234567",
        )
        .expect("source-bound contract fixture encodes");
        let value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("semantic envelope decodes");
        assert_eq!(value["evidence"]["production_acceptance_eligible"], false);
        assert_eq!(
            value["evidence"]["resolver_quality"]["holdout_available"],
            false
        );
    }
}
