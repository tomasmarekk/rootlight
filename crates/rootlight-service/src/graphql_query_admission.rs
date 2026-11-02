//! Unknown `.query` sources need native operation evidence before GraphQL routing.
//! Complete parser coverage and the installed analyzer/provider pair bind that
//! decision to the whole authenticated snapshot, without changing discovery hints.

use super::*;
use rootlight_adapter_sdk::{MemoryAdmissionStatus, SyntaxFactKind, execute_parse};

const MAX_PROBE_BYTES: usize = 64 * 1024;

#[cfg(test)]
thread_local! {
    pub(super) static PREDECESSOR_POLICY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn predecessor_policy() -> bool {
    PREDECESSOR_POLICY.with(std::cell::Cell::get)
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Admission {
    NotAttempted,
    NoEvidence,
    Accepted {
        operation: SourceSpan,
        memory_admission: MemoryAdmissionStatus,
    },
    ResourceBounded,
    InvalidEncoding,
}

fn query_suffix(input: &ManifestInput) -> bool {
    Path::new(&input.path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("query"))
}

pub(super) fn probe(
    service: &FirstSliceService,
    repository: RepositoryId,
    generation: GenerationId,
    input: &ManifestInput,
    snapshot: &SourceSnapshot,
    cancellation: &Cancellation,
) -> Result<Admission, FirstSliceError> {
    check_cancellation(cancellation)?;
    #[cfg(test)]
    if predecessor_policy() {
        return Ok(Admission::NotAttempted);
    }
    if !query_suffix(input)
        || !input.language_signals.is_empty()
        || !input.project_language_witnesses.is_empty()
    {
        return Ok(Admission::NotAttempted);
    }
    if snapshot.file() != input.file
        || snapshot.path().as_str() != input.path
        || snapshot.content_hash() != input.content_hash
        || usize::try_from(input.bytes).ok() != Some(snapshot.content().len())
    {
        return Err(FirstSliceError::Identity);
    }
    let Some(analyzer) = service.analyzers.get("graphql") else {
        return Ok(Admission::NotAttempted);
    };
    let parser = service
        .structural_parsers
        .get("graphql")
        .ok_or(FirstSliceError::Adapter)?;
    if !analyzer.uses_parse_provider(parser.as_ref()) {
        return Err(FirstSliceError::Identity);
    }
    if snapshot.content().len() > MAX_PROBE_BYTES
        || snapshot.content().len() > service.analysis_limits.max_source_bytes()
        || snapshot.content().len() > parser.capabilities().max_source_bytes()
    {
        return Ok(Admission::ResourceBounded);
    }
    let encoding = EncodingId::new(&input.encoding).map_err(|_| FirstSliceError::Adapter)?;
    if !analyzer.supports_encoding(&encoding) {
        return Ok(Admission::InvalidEncoding);
    }
    let limits = provider_analysis_limits(&service.analysis_limits, parser.capabilities())?;
    let request = structural_analysis_request(
        repository,
        generation,
        snapshot,
        matches!(input.class, InputClass::Generated),
        "graphql",
        &input.encoding,
        &limits,
    )?;
    let request = request.to_parse_request();
    // Language evidence follows the host's explicit fallback policy; it does
    // not strengthen the provider's memory-enforcement capability.
    let memory_policy = match parser.capabilities().memory_enforcement() {
        rootlight_adapter_sdk::MemoryEnforcement::Unavailable => {
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback
        }
        _ => MemoryAdmissionPolicy::RequireHardOrAccounted,
    };
    let output = match execute_parse(parser.as_ref(), &request, memory_policy, cancellation) {
        Ok(output) => output,
        Err(error) if is_invalid_utf8_adapter_failure(&error) => {
            check_cancellation(cancellation)?;
            return Ok(Admission::InvalidEncoding);
        }
        Err(AdapterError::ProviderFailed { code })
            if matches!(
                code.as_str(),
                "syntax-node-parse-work-limit" | "syntax-depth-parse-work-limit"
            ) =>
        {
            check_cancellation(cancellation)?;
            return Ok(Admission::ResourceBounded);
        }
        Err(error) => {
            return match map_adapter_error(error, cancellation) {
                FirstSliceError::ResourceLimit { .. } => Ok(Admission::ResourceBounded),
                error => Err(error),
            };
        }
    };
    check_cancellation(cancellation)?;
    let coverage = output.report().coverage();
    if coverage.status() != CoverageStatus::Complete
        || coverage.covered_source_bytes() != snapshot.content().len()
        || coverage.total_source_bytes() != snapshot.content().len()
        || coverage.skipped_regions() != 0
        || output
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.coverage_effect() != CoverageStatus::Complete)
        || output
            .facts()
            .iter()
            .any(|fact| fact.kind() == SyntaxFactKind::ErrorRecovery)
    {
        return Ok(Admission::NoEvidence);
    }
    let source = request.source().source_ref();
    if !request.included_ranges().is_empty()
        || source.span().start_byte() != 0
        || usize::try_from(source.span().end_byte()).ok() != Some(snapshot.content().len())
        || source.content_hash() != snapshot.content_hash()
    {
        return Err(FirstSliceError::Identity);
    }
    for (kind, label) in [
        (SyntaxFactKind::Root, "graphql.file.root"),
        (SyntaxFactKind::Module, "graphql.file.module"),
    ] {
        if output
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == kind
                    && fact.syntax_kind().as_str() == label
                    && fact.span() == source.span()
            })
            .count()
            != 1
        {
            return Ok(Admission::NoEvidence);
        }
    }
    for fact in output.facts() {
        check_cancellation(cancellation)?;
        if !matches!(
            (fact.kind(), fact.syntax_kind().as_str()),
            (SyntaxFactKind::Scope, "graphql.anonymous_owner.scope")
                | (SyntaxFactKind::Declaration, "graphql.operation.declaration")
        ) {
            continue;
        }
        let span = fact.span();
        if span.file() != snapshot.file() {
            return Err(FirstSliceError::Identity);
        }
        let Some(bytes) = usize::try_from(span.start_byte())
            .ok()
            .zip(usize::try_from(span.end_byte()).ok())
            .and_then(|(start, end)| snapshot.content().get(start..end))
        else {
            return Err(FirstSliceError::Identity);
        };
        if [b"query".as_slice(), b"mutation", b"subscription"]
            .iter()
            .any(|keyword| {
                bytes.starts_with(keyword)
                    && bytes
                        .get(keyword.len())
                        .is_some_and(|next| !next.is_ascii_alphanumeric() && *next != b'_')
            })
        {
            return Ok(Admission::Accepted {
                operation: span,
                memory_admission: output.memory_admission(),
            });
        }
    }
    Ok(Admission::NoEvidence)
}

pub(super) fn route_hash(
    input: &ManifestInput,
    language: Option<&str>,
    admission: Admission,
    previous: Option<ContentHash>,
) -> Result<Option<ContentHash>, FirstSliceError> {
    #[cfg(test)]
    if predecessor_policy() {
        return Ok(previous);
    }
    if !query_suffix(input) {
        return Ok(previous);
    }
    let state: &[u8] = match admission {
        Admission::NotAttempted => b"not-attempted",
        Admission::NoEvidence => b"no-evidence",
        Admission::Accepted { .. } => b"accepted",
        Admission::ResourceBounded => b"resource-bounded",
        Admission::InvalidEncoding => b"invalid-encoding",
    };
    let mut hasher = blake3::Hasher::new();
    hash_component(
        &mut hasher,
        b"rootlight.file-parser-route/native-graphql-query/1",
    )?;
    hash_component(&mut hasher, language.unwrap_or("unknown").as_bytes())?;
    hash_component(
        &mut hasher,
        detected_source_language(input)
            .unwrap_or("unknown")
            .as_bytes(),
    )?;
    hash_component(&mut hasher, state)?;
    if let Some(previous) = previous {
        hash_component(&mut hasher, previous.as_bytes())?;
    }
    if let Admission::Accepted {
        operation,
        memory_admission,
    } = admission
    {
        let memory: &[u8] = match memory_admission {
            MemoryAdmissionStatus::HardProcess => b"hard-process",
            MemoryAdmissionStatus::AccountedInProcess => b"accounted-in-process",
            MemoryAdmissionStatus::UnavailableEnforcementFallback => {
                b"unavailable-enforcement-fallback"
            }
            _ => return Err(FirstSliceError::Adapter),
        };
        hash_component(&mut hasher, memory)?;
        // Content identity is already an input; generation must not prevent reuse.
        hash_component(&mut hasher, &operation.start_byte().to_le_bytes())?;
        hash_component(&mut hasher, &operation.end_byte().to_le_bytes())?;
    }
    Ok(Some(ContentHash::from_bytes(*hasher.finalize().as_bytes())))
}
