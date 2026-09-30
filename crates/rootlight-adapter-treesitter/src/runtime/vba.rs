//! Closed first-party procedural modules enter the existing syntax normalizer.
//! Unsupported source returns to the native whole-module path; resource failures
//! never trigger a second parser, and successful scans retain no native tree.

use std::ops::Range;

use rootlight_adapter_sdk::NativeVbaModuleKind;
use rootlight_vba::{
    ModuleBodyStatement, ModuleError, ModuleInventory, ModuleLimits, ModuleLocalKind,
    ModuleScalarValueKind, ProcedureKind,
};

use super::*;
use crate::VBA_PROCEDURAL_FRONTEND_VERSION;

pub(super) struct PreparedModule {
    inventory: ModuleInventory,
    family: GrammarFamily,
    native_family: GrammarFamily,
    processed_nodes: usize,
}

pub(super) fn prepare(
    provider: &TreeSitterProvider,
    request: &ParseRequest<'_>,
    previous: Option<&PreviousParse>,
    edits: &[SourceEdit],
    settings: ParserSettings,
    cancellation: &Cancellation,
) -> Result<Option<PreparedModule>, AdapterError> {
    cancellation.check()?;
    if !matches!(request.language().as_str(), "vba" | "visual-basic-classic")
        || request.encoding().as_str() != "utf-8"
        || request.native_vba_module() == Some(NativeVbaModuleKind::Object)
        || !request.included_ranges().is_empty()
    {
        return Ok(None);
    }
    let (family, native_family) =
        provider.admit_parse_input(request, previous, edits, settings, cancellation)?;
    let maximum_nodes = request.limits().max_syntax_nodes();
    let maximum_source = request
        .limits()
        .max_source_bytes()
        .min(provider.config.max_source_bytes());
    let limits = ModuleLimits::new(maximum_source, maximum_nodes, maximum_nodes)
        .with_max_parameters(maximum_nodes);
    let inventory =
        match rootlight_vba::module_inventory(request.source().bytes(), limits, &mut || {
            cancellation.check()
        }) {
            Ok(inventory) => inventory,
            Err(ModuleError::Unsupported { .. }) => return Ok(None),
            Err(ModuleError::SourceLimit) => {
                return Err(RequestError::ProviderLimit {
                    resource: ResourceKind::SourceBytes,
                    observed: request.source().bytes().len(),
                    limit: maximum_source,
                }
                .into());
            }
            Err(
                ModuleError::StatementLimit
                | ModuleError::ProcedureLimit
                | ModuleError::ParameterLimit,
            ) => {
                return Err(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: maximum_nodes
                        .checked_add(1)
                        .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?,
                    limit: maximum_nodes,
                }
                .into());
            }
            Err(ModuleError::AllocationFailed) => return Err(SinkError::AllocationFailed.into()),
            Err(ModuleError::Cancelled(cancelled)) => return Err(cancelled.into()),
        };
    let mut processed_nodes = inventory
        .statement_count()
        .checked_add(inventory.procedures().len())
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
    for procedure in inventory.procedures() {
        cancellation.check()?;
        processed_nodes = processed_nodes
            .checked_add(procedure.parameters().len())
            .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
    }
    require_provider_limit(ResourceKind::SyntaxNodes, processed_nodes, maximum_nodes)?;
    cancellation.check()?;
    Ok(Some(PreparedModule {
        inventory,
        family,
        native_family,
        processed_nodes,
    }))
}

pub(super) fn required_facts(
    prepared: PreparedModule,
    request: &ParseRequest<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let candidates = candidates(
        &prepared.inventory,
        request.source().bytes(),
        false,
        request.limits().max_syntax_nodes(),
        cancellation,
    )?;
    let normalized = normalize_query_candidates(candidates, request, usize::MAX, cancellation)?;
    validate_depth(&normalized, request, cancellation)?;
    Ok(normalized.required)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit(
    prepared: PreparedModule,
    request: &ParseRequest<'_>,
    previous: Option<&PreviousParse>,
    edits: &[SourceEdit],
    settings: ParserSettings,
    sink: &mut dyn SyntaxFactSink,
    cancellation: &Cancellation,
) -> Result<RawParseWithPrevious, AdapterError> {
    let candidates = candidates(
        &prepared.inventory,
        request.source().bytes(),
        true,
        request.limits().max_syntax_nodes(),
        cancellation,
    )?;
    let normalized = normalize_query_candidates(
        candidates,
        request,
        pre_extraction_fact_limit(sink.remaining_budget())?,
        cancellation,
    )?;
    let maximum_depth = validate_depth(&normalized, request, cancellation)?;
    if normalized.limited {
        emit_extraction_limit_diagnostic(request, sink, cancellation)?;
    }
    let plan = plan_fact_batches(&normalized.facts, sink.remaining_budget(), cancellation)?;
    // The closed route knows every retained fact before publication. Exact
    // byte/batch clipping must not discard a later declaration's identity.
    if plan.emitted < normalized.facts.len() {
        return Err(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed: normalized.facts.len(),
            limit: plan.emitted,
        }
        .into());
    }
    let limited = normalized.limited;
    emit_fact_plan(normalized.facts, plan, sink, cancellation)?;
    let source_len = request.source().bytes().len();
    let covered = if request.included_ranges().is_empty() {
        source_len
    } else {
        included_range_source_bytes(request.included_ranges(), cancellation)?
    };
    let omitted = covered < source_len;
    let usage = sink.staged_usage();
    let coverage = CoverageReport::new(
        AnalysisTier::TierD,
        if limited || omitted {
            CoverageStatus::Bounded
        } else {
            CoverageStatus::Complete
        },
        source_len,
        covered,
        usize::from(limited)
            .checked_add(usize::from(omitted))
            .ok_or_else(|| provider_failure("coverage-accounting"))?,
        Vec::new(),
    )
    .map_err(AdapterError::InvalidReport)?;
    let report = WorkReport::new(
        coverage,
        ResourceUsage::new(
            source_len,
            usage.records(),
            prepared.processed_nodes,
            maximum_depth,
            None,
            usage,
        ),
        StreamEnd::new(sink.next_sequence(), usage),
    )
    .map_err(AdapterError::InvalidReport)?;
    cancellation.check()?;
    Ok(RawParseWithPrevious {
        report,
        pending: None,
        reuse_status: if previous.is_some() {
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        } else {
            ReuseStatus::Fresh
        },
        reuse_key: ParseReuseKey {
            previous_content_hash: None,
            current_content_hash: request.source().source_ref().content_hash(),
            family: prepared.family,
            native_family: prepared.native_family,
            grammar_version: VBA_PROCEDURAL_FRONTEND_VERSION,
            encoding: request.encoding().as_str().to_owned(),
            included_ranges: request.shared_included_ranges(),
            settings,
            edits: source_edit_identities(edits, cancellation)?,
        },
    })
}

fn validate_depth(
    normalized: &NormalizedFacts,
    request: &ParseRequest<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut maximum = 0;
    for (index, fact) in normalized.facts.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        maximum = maximum.max(fact.depth());
    }
    require_provider_limit(
        ResourceKind::SyntaxDepth,
        maximum,
        request.limits().max_syntax_depth(),
    )?;
    cancellation.check()?;
    Ok(maximum)
}

fn candidates(
    inventory: &ModuleInventory,
    source: &[u8],
    include_optional: bool,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Vec<QueryCandidate>, AdapterError> {
    if inventory.source_len() != source.len() {
        return Err(provider_failure("vba-procedural-source-identity"));
    }
    let mut count = inventory
        .procedures()
        .len()
        .checked_mul(4)
        .and_then(|count| count.checked_add(3 + usize::from(inventory.module_name().is_some())))
        .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
    for procedure in inventory.procedures() {
        cancellation.check()?;
        count = procedure
            .parameters()
            .len()
            .checked_mul(2)
            .and_then(|parameters| count.checked_add(parameters))
            .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
        for statement in procedure.statements() {
            cancellation.check()?;
            if matches!(statement, ModuleBodyStatement::Declaration(_)) {
                count = count
                    .checked_add(2)
                    .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
            }
        }
    }
    require_provider_limit(ResourceKind::SyntaxNodes, count, maximum)?;
    let mut output = Vec::new();
    try_reserve_exact_cancellable(
        &mut output,
        count,
        cancellation,
        "vba-procedural-fact-allocation",
    )?;
    for role in [
        StructuralRole::Root,
        StructuralRole::Module,
        StructuralRole::Scope,
    ] {
        output.push(candidate(
            source_root_start(source, cancellation)?..source.len(),
            role,
            "vba.file",
            role != StructuralRole::Scope,
        ));
    }
    if let Some(name) = inventory.module_name() {
        let start = name
            .start
            .checked_sub(1)
            .ok_or_else(|| provider_failure("vba-procedural-module-name"))?;
        let end = name
            .end
            .checked_add(1)
            .ok_or_else(|| provider_failure("vba-procedural-module-name"))?;
        let text = source
            .get(start..end)
            .and_then(|text| std::str::from_utf8(text).ok())
            .ok_or_else(|| provider_failure("vba-procedural-module-name"))?;
        if rootlight_adapter_sdk::vba_module_name(text).is_none() {
            return Err(provider_failure("vba-procedural-module-name"));
        }
        output.push(candidate(
            start..end,
            StructuralRole::Definition,
            "vba.module_name",
            true,
        ));
    }
    for procedure in inventory.procedures() {
        cancellation.check()?;
        let syntax = match procedure.kind() {
            ProcedureKind::Sub => "vba.sub_function",
            ProcedureKind::Function => "vba.function",
            _ => return Err(provider_failure("vba-procedural-kind")),
        };
        let header = native_header(
            source,
            procedure.header(),
            procedure.body(),
            procedure.declaration(),
            procedure
                .statements()
                .first()
                .map(|statement| statement.statement().start),
            cancellation,
        )?;
        output.push(candidate(
            procedure.declaration(),
            StructuralRole::Declaration,
            syntax,
            true,
        ));
        output.push(candidate(
            procedure.declaration(),
            StructuralRole::Scope,
            syntax,
            false,
        ));
        output.push(candidate(
            header.clone(),
            StructuralRole::Signature,
            syntax,
            true,
        ));
        output.push(candidate(
            procedure.name(),
            StructuralRole::Definition,
            "vba.identifier",
            true,
        ));
        for parameter in procedure.parameters() {
            cancellation.check()?;
            push_body_candidate(
                &mut output,
                maximum,
                cancellation,
                parameter.declaration(),
                StructuralRole::Declaration,
                "vba.parameter",
                true,
            )?;
            push_body_candidate(
                &mut output,
                maximum,
                cancellation,
                parameter.name(),
                StructuralRole::Definition,
                "vba.identifier",
                true,
            )?;
        }
        if include_optional {
            let text = source
                .get(procedure.header())
                .ok_or_else(|| provider_failure("vba-procedural-header"))?;
            let first = text
                .split(|byte| byte.is_ascii_whitespace())
                .next()
                .unwrap_or_default();
            let visibility = if first.eq_ignore_ascii_case(b"Public") {
                "vba.public"
            } else if first.eq_ignore_ascii_case(b"Private") {
                "vba.private"
            } else if first.eq_ignore_ascii_case(b"Friend") {
                "vba.project_restricted"
            } else {
                "vba.visibility_unknown"
            };
            require_provider_limit(
                ResourceKind::SyntaxNodes,
                output
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?,
                maximum,
            )?;
            try_reserve_cancellable(
                &mut output,
                1,
                cancellation,
                "vba-procedural-fact-allocation",
            )?;
            output.push(candidate(
                header,
                StructuralRole::Expression,
                visibility,
                false,
            ));
        }
        for statement in procedure.statements() {
            body_candidates(
                statement,
                &mut output,
                include_optional,
                maximum,
                cancellation,
            )?;
        }
    }
    if include_optional {
        lexical_trivia(source, &mut output, maximum, cancellation)?;
    }
    cancellation.check()?;
    Ok(output)
}

fn body_candidates(
    statement: &ModuleBodyStatement,
    output: &mut Vec<QueryCandidate>,
    include_optional: bool,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    match statement {
        ModuleBodyStatement::Declaration(declaration) => {
            let syntax = match declaration.kind() {
                ModuleLocalKind::Variable => "vba.variable",
                ModuleLocalKind::Constant => "vba.constant",
            };
            push_body_candidate(
                output,
                maximum,
                cancellation,
                declaration.declarator(),
                StructuralRole::Declaration,
                syntax,
                true,
            )?;
            push_body_candidate(
                output,
                maximum,
                cancellation,
                declaration.name(),
                StructuralRole::Definition,
                "vba.identifier",
                true,
            )?;
            if include_optional {
                push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    declaration.declarator(),
                    StructuralRole::Expression,
                    "vba.visibility_unknown",
                    false,
                )?;
            }
        }
        ModuleBodyStatement::Assignment(assignment) if include_optional => {
            push_body_candidate(
                output,
                maximum,
                cancellation,
                assignment.destination(),
                StructuralRole::Reference,
                "vba.let_identifier",
                false,
            )?;
            match assignment.value().kind() {
                ModuleScalarValueKind::Name => push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    assignment.value().span(),
                    StructuralRole::Reference,
                    "vba.value_identifier",
                    false,
                )?,
                // Numeric and Boolean nodes have no captures in the current query
                // pack; quoted strings use the shared lexical-trivia scan below.
                ModuleScalarValueKind::UnsignedDecimal
                | ModuleScalarValueKind::String
                | ModuleScalarValueKind::Boolean => {}
            }
        }
        ModuleBodyStatement::Call(call) if include_optional => {
            push_body_candidate(
                output,
                maximum,
                cancellation,
                call.statement(),
                StructuralRole::Reference,
                "vba.application",
                false,
            )?;
            push_body_candidate(
                output,
                maximum,
                cancellation,
                call.callee(),
                StructuralRole::Reference,
                "vba.invoked_identifier",
                false,
            )?;
        }
        ModuleBodyStatement::Assignment(_) | ModuleBodyStatement::Call(_) => {}
    }
    Ok(())
}

fn push_body_candidate(
    output: &mut Vec<QueryCandidate>,
    maximum: usize,
    cancellation: &Cancellation,
    range: Range<usize>,
    role: StructuralRole,
    syntax: &'static str,
    required: bool,
) -> Result<(), AdapterError> {
    require_provider_limit(
        ResourceKind::SyntaxNodes,
        output
            .len()
            .checked_add(1)
            .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?,
        maximum,
    )?;
    try_reserve_cancellable(output, 1, cancellation, "vba-procedural-fact-allocation")?;
    output.push(candidate(range, role, syntax, required));
    Ok(())
}

fn candidate(
    range: Range<usize>,
    role: StructuralRole,
    syntax: &'static str,
    required: bool,
) -> QueryCandidate {
    QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required,
        native_depth: 0,
    }
}

fn source_root_start(source: &[u8], cancellation: &Cancellation) -> Result<usize, AdapterError> {
    // Native extras before the first source-file item do not extend its span.
    // A newline is a real item and must not be trimmed as leading whitespace.
    for (offset, byte) in source.iter().enumerate() {
        if offset.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if !matches!(byte, b' ' | b'\t' | b'\x0c') {
            return Ok(offset);
        }
    }
    cancellation.check()?;
    Ok(source.len())
}

fn native_header(
    source: &[u8],
    header: Range<usize>,
    body: Range<usize>,
    declaration: Range<usize>,
    first_statement: Option<usize>,
    cancellation: &Cancellation,
) -> Result<Range<usize>, AdapterError> {
    let suffix = source
        .get(header.end..body.start)
        .ok_or_else(|| provider_failure("vba-procedural-header"))?;
    let mut end = declaration.end;
    for (offset, byte) in suffix.iter().enumerate() {
        cancellation.check()?;
        if *byte == b'\n' {
            end = header.end + offset;
            break;
        }
    }
    if end == declaration.end {
        let boundary = first_statement.unwrap_or(body.end);
        if first_statement.is_some() {
            end = boundary;
        }
        let body_bytes = source
            .get(body.start..boundary)
            .ok_or_else(|| provider_failure("vba-procedural-header"))?;
        let mut comment = false;
        for (offset, byte) in body_bytes.iter().enumerate() {
            cancellation.check()?;
            if *byte == b'\n' || (*byte == b':' && !comment) {
                end = body.start + offset;
                break;
            }
            comment |= *byte == b'\'';
        }
    }
    while end > declaration.start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
        cancellation.check()?;
        end -= 1;
    }
    Ok(declaration.start..end)
}

fn lexical_trivia(
    source: &[u8],
    output: &mut Vec<QueryCandidate>,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut cursor = 0;
    while let Some(byte) = source.get(cursor) {
        cancellation.check()?;
        let start = cursor;
        let quoted = *byte == b'"';
        if quoted {
            cursor += 1;
            loop {
                cancellation.check()?;
                let Some(next) = source.get(cursor) else {
                    return Err(provider_failure("vba-procedural-string"));
                };
                cursor += 1;
                if *next == b'"' {
                    if source.get(cursor) != Some(&b'"') {
                        break;
                    }
                    cursor += 1;
                }
            }
        } else if *byte == b'[' {
            cursor += 1;
            while source.get(cursor).is_some_and(|byte| *byte != b']') {
                cancellation.check()?;
                cursor += 1;
            }
            if source.get(cursor) != Some(&b']') {
                return Err(provider_failure("vba-procedural-bracket"));
            }
            cursor += 1;
            continue;
        } else if *byte == b'\''
            || (source
                .get(cursor..cursor.saturating_add(3))
                .is_some_and(|word| word.eq_ignore_ascii_case(b"Rem"))
                && (cursor == 0
                    || source
                        .get(cursor - 1)
                        .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b':'))
                && source
                    .get(cursor + 3)
                    .is_none_or(|byte| byte.is_ascii_whitespace()))
        {
            while source.get(cursor).is_some_and(|byte| *byte != b'\n') {
                cancellation.check()?;
                cursor += 1;
            }
        } else {
            cursor += 1;
            continue;
        }
        require_provider_limit(
            ResourceKind::SyntaxNodes,
            output
                .len()
                .checked_add(1)
                .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?,
            maximum,
        )?;
        try_reserve_cancellable(output, 1, cancellation, "vba-procedural-fact-allocation")?;
        output.push(candidate(
            start..cursor,
            if quoted {
                StructuralRole::StringLiteral
            } else {
                StructuralRole::Comment
            },
            if quoted { "vba.string" } else { "vba.comment" },
            false,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::{
        AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot, StreamLimits,
    };
    use rootlight_ir::{BuildContextIdentity, IrLimits};
    use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};

    struct Fixture {
        _directory: tempfile::TempDir,
        snapshot: SourceSnapshot,
        source: SourceRef,
        limits: AnalysisLimits,
        provider: TreeSitterProvider,
    }

    impl Fixture {
        fn new(text: &str) -> Self {
            let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
            std::fs::write(directory.path().join("module.bas"), text).unwrap();
            let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
            let repository = RepositoryRoot::open(repository_id, directory.path()).unwrap();
            let relative = RelativePath::parse(std::path::Path::new("module.bas")).unwrap();
            let snapshot = repository.snapshot(&relative, 1024 * 1024).unwrap();
            let source = SourceRef::new(
                repository_id,
                "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                    .parse()
                    .unwrap(),
                SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
                snapshot.content_hash(),
                None,
            );
            let batch = BatchThresholds::new(128, 256 * 1024, 8, 4096).unwrap();
            let stream =
                StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch)
                    .unwrap();
            let limits = AnalysisLimits::new(
                1024 * 1024,
                100_000,
                64,
                16,
                8 * 1024 * 1024,
                stream.clone(),
                stream,
                IrLimits::default(),
            )
            .unwrap();
            let config = RuntimeConfig::new(
                1024 * 1024,
                100_000,
                64,
                16,
                16,
                1,
                2 * 1024 * 1024,
                ParserSettings::new(1024).unwrap(),
            )
            .unwrap();
            Self {
                _directory: directory,
                snapshot,
                source,
                limits,
                provider: TreeSitterProvider::new(config).unwrap(),
            }
        }

        fn request(&self, language: &str, ranges: &[(u64, u64)]) -> ParseRequest<'_> {
            let ranges = ranges
                .iter()
                .map(|&(start, end)| {
                    IncludedRange::new(
                        SourceSpan::new(self.snapshot.file(), start, end).unwrap(),
                        LanguageId::new(language).unwrap(),
                    )
                })
                .collect();
            ParseRequest::new(
                GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
                LanguageId::new(language).unwrap(),
                EncodingId::utf8(),
                ranges,
                &self.limits,
            )
            .unwrap()
        }
    }

    fn compare_native(fixture: &Fixture, language: &str, ranges: &[(u64, u64)]) {
        let cancellation = Cancellation::new();
        let request = fixture.request(language, ranges);
        let prepared = prepare(
            &fixture.provider,
            &request,
            None,
            &[],
            fixture.provider.config.default_settings(),
            &cancellation,
        )
        .unwrap()
        .expect("the authored complete procedural module is supported");
        let identity = normalize_query_candidates(
            candidates(
                &prepared.inventory,
                request.source().bytes(),
                false,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap(),
            &request,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        let actual = normalize_query_candidates(
            candidates(
                &prepared.inventory,
                request.source().bytes(),
                true,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap(),
            &request,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        assert_eq!(fixture.provider.stats().pooled_parsers, 0);
        assert_eq!(fixture.provider.stats().cache.entries, 0);
        // This private legacy path bypasses routing without a public test flag.
        let native = fixture
            .provider
            .prepare_tree(
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap();
        assert!(native.traversal.fully_traversed);
        assert_eq!(
            native.traversal.coverage,
            if ranges.is_empty() {
                CoverageStatus::Complete
            } else {
                CoverageStatus::Bounded
            },
            "the closed authored module has no native recovery: {:?}",
            request.source().bytes()
        );
        let pack = fixture
            .provider
            .query_packs
            .get_for_source(native.family, request.source().path().as_str())
            .unwrap();
        let legacy = pack
            .extract(
                native.family,
                &native.tree,
                request.source().bytes(),
                request.limits().max_syntax_nodes(),
                usize::MAX,
                &cancellation,
            )
            .unwrap();
        assert!(legacy.limit.is_none());
        let expected =
            normalize_query_candidates(legacy.candidates, &request, usize::MAX, &cancellation)
                .unwrap();
        let expected_identity = normalize_query_candidates(
            pack.extract_identity(
                native.family,
                &native.tree,
                request.source().bytes(),
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap(),
            &request,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        assert_eq!(
            identity.required, expected_identity.required,
            "identity demand for {language}"
        );
        assert_eq!(
            actual.required, expected.required,
            "full demand for {language}"
        );
        assert_eq!(
            actual.facts,
            expected.facts,
            "exact source facts for {language}, {:?}",
            request.source().bytes()
        );
    }

    #[test]
    fn first_party_vba_module_facts_match_the_private_native_route() {
        let cases = [
            "",
            " ",
            " \t\x0c",
            " \t\x0cSub Run()\nEnd Sub\n",
            "Sub Run()\nEnd Sub",
            "Sub Run()\nEnd Sub \t\x0c",
            "Sub Run()\nEnd Sub\n \t\x0c",
            "\n' leading\nRem ordinary comment\n\n",
            "' ordinary comment:",
            "Rem ordinary comment:",
            "Sub Run()\n' body:\nEnd Sub\n' trailing:",
            "Attribute VB_Name = \"ModuleA\"\nOption Explicit\nPublic Sub Run()\nEnd Sub\n",
            "  ' leading\r\nAttribute VB_Name = \"ModuleA\"\r\nOption Compare Text\r\nPrivate Static Sub [Run]() ' header\r\n' body\r\nEnd Sub\r\n",
            "Function Result$()\nEnd Function\nPublic Function Count() As Long\nEnd Function\n",
            "Sub First(): End Sub\nSub Second(): : End Sub\n",
            "Sub Same()\nEnd Sub\nSub Same()\nEnd Sub\n",
            "Public _\r\nSub Run( _\r\n)\r\nEnd Sub\r\n",
            "Sub Run() ' header\r\nEnd Sub ' terminator\r\n",
            "Sub Run(): ' body\r\nEnd Sub ' terminator\r\n",
        ];
        for source in cases {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        for keyword in [
            "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
            "Double", "Currency", "Date", "Variant", "Object",
        ] {
            let source = format!("Function Result() As {keyword}\nEnd Function\n");
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(&source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_scalar_bodies_match_the_private_native_route() {
        let cases = [
            "Sub Run()\nDim value As Long\nConst seed As Long = 65\nvalue = seed\nLet value = 7\nEnd Sub\n",
            "Sub Run()\nDim [value] As String\nConst [text] As String = \"a\"\"b\"\n[value] = [text]\nLet [value] = \"replacement\"\nEnd Sub\n",
            "Sub Run()\nDim flag As Boolean\nConst enabled As Boolean = True\nflag = enabled\nLet flag = False\nEnd Sub\n",
            "Sub Run()\nDim [Me] As Boolean\nConst [True] As Boolean = False\n[Me] = [True]\nDim [Rem] As Long\nConst [Sub] As Long = 1\n[Rem] = [Sub]\nEnd Sub\n",
            "Attribute VB_Name = \"ModuleA\"\r\nOption Explicit\r\nPrivate Static Function Result() As Long\r\nDim count As Long ' declaration\r\nConst seed As Long = 7 ' constant\r\nLet count = seed ' assignment\r\nResult = count\r\nEnd Function ' terminator\r\n",
            "Sub Run():Dim value As Long:Const seed As Long = 7:value = seed:Let value = 8:End Sub\n",
            "Sub Run(): : Dim value As Long\n' body:\nLet value = 1\nEnd Sub\n",
            "Sub Run(): ' header\r\nDim value As Long: ' declaration\r\nLet value = 1\r\nEnd Sub\r\n",
            "Sub Run()\nDim _\r\n [value] As _\n Long\nConst [seed] As _\r\n Long = _\n 7\nLet _\r\n [value] = _\n [seed]\nEnd Sub\n",
            "Function Result() As String\nDim value As String\nConst seed As String = \"\"\nvalue = \"text\"\nResult = value\nEnd Function\n",
            "Sub Run()\nDim value As Long\nConst seed As Long = 001\nvalue = 000\nEnd Sub",
        ];
        for source in cases {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        for keyword in [
            "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
            "Double", "Currency", "Date", "Variant", "Object",
        ] {
            let source = format!(
                "Sub Run()\nDim value As {keyword}\nConst seed As {keyword} = 1\nvalue = seed\nEnd Sub\n"
            );
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(&source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_intrinsic_parameters_match_the_private_native_route() {
        for passing in ["", "ByVal ", "ByRef "] {
            for intrinsic in [
                "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
                "Double", "Currency", "Date", "Variant", "Object",
            ] {
                for (kind, body, result) in [
                    ("Sub", "value = value", ""),
                    ("Function", "Result = value", " As Long"),
                ] {
                    let source = format!(
                        "{kind} Result({passing}value As {intrinsic}){result}\n{body}\nEnd {kind}\n"
                    );
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        for source in [
            "Sub Run(ByVal [Me] As Long):[Me] = [Me]:End Sub\n",
            "Sub Run(ByRef [Rem] As Long) ' header\r\n[Rem] = [Rem] ' body\r\nEnd Sub ' terminator\r\n",
            "Public _\r\nFunction Result( _\n bYvAl _\r\n [value] _\n As _\r\n Long _\n ) As Long\r\nResult = [value]\r\nEnd Function\r\n",
            "Sub First(ByVal value As Long)\nvalue = value\nEnd Sub\nSub Second(ByRef value As Long)\nvalue = value\nEnd Sub\n",
            "Sub Accept(ByVal value As Long)\nEnd Sub\nSub Run()\nCall Accept()\nEnd Sub\n",
            "Sub Run(ByVal first As Long, ByRef second As String)\nfirst = first\nsecond = second\nEnd Sub\n",
            "Sub Run(value As Long, value As Long)\nvalue = value\nEnd Sub\n",
            "Function Result(first As Long, ByVal second As String, ByRef third As Boolean) As Long\nResult = first\nEnd Function\n",
            "Sub Run( _\r\n ByVal first As Long, _\n ByRef [second] As String, _\r\n third As Object _\n )\nfirst = first\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_parameter_positions_never_gain_false_complete_admission() {
        let mut unexpected_admissions = Vec::new();
        for name in [
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
        ] {
            for passing in ["", "ByVal ", "ByRef "] {
                let source = format!("Sub Run({passing}{name} As Long)\nEnd Sub\n");
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let cancellation = Cancellation::new();
                    let native = fixture
                        .provider
                        .prepare_tree(
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap();
                    assert!(native.traversal.fully_traversed);
                    let admitted = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_some();
                    if native.traversal.coverage == CoverageStatus::Unknown {
                        if admitted {
                            unexpected_admissions.push(format!("{language}:{passing}{name}"));
                        }
                    } else {
                        assert_eq!(
                            native.traversal.coverage,
                            CoverageStatus::Complete,
                            "the parameter-position oracle must not be bounded: {language}, {passing}{name}"
                        );
                        assert!(
                            admitted,
                            "native-complete parameter names belong to the closed slice: {language}, {passing}{name}"
                        );
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        assert!(
            unexpected_admissions.is_empty(),
            "native-recovered parameter names retain whole-module fallback: {unexpected_admissions:?}"
        );
    }

    #[test]
    fn first_party_vba_unsupported_parameters_keep_whole_native_fallback() {
        for parameters in [
            "Optional value As Long = 1",
            "ByVal value As Long = 1",
            "ParamArray values() As Variant",
            "ByRef values() As Long",
            "ByVal value As UserType",
            "ByVal value As [Types].[Result]",
            "ByVal value As New Object",
            "ByVal value As String * 4",
            "ByVal value",
            "ByVal ByRef value As Long",
            "ByVal value$ As String",
            "ByVal first As Long,",
            ", ByVal value As Long",
            "ByVal first As Long,, ByRef second As Long",
            "ByVal first As Long, Optional second As Long = 1",
            "ByVal first As Long, ByRef second As UserType",
        ] {
            let source = format!("Sub Empty()\nEnd Sub\nSub Run({parameters})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "an unsupported parameter never publishes a supported prefix: {language}, {parameters}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    fn with_maximum_nodes(limits: &AnalysisLimits, maximum: usize) -> AnalysisLimits {
        AnalysisLimits::new(
            limits.max_source_bytes(),
            maximum,
            limits.max_syntax_depth(),
            limits.max_embedded_ranges(),
            limits.max_reported_memory_bytes(),
            limits.syntax_stream().clone(),
            limits.ir_stream().clone(),
            limits.ir().clone(),
        )
        .unwrap()
    }

    #[test]
    fn first_party_vba_parameter_nodes_and_candidate_ceilings_are_exact() {
        for (parameters, parameter_count, required) in [
            ("ByVal value As Long", 1, 9),
            ("ByVal first As Long, ByRef second As String", 2, 11),
        ] {
            let source = format!("Sub Run({parameters})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let mut fixture = Fixture::new(&source);
                let cancellation = Cancellation::new();
                let request = fixture.request(language, &[]);
                let prepared = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    prepared.inventory.procedures()[0].parameters().len(),
                    parameter_count
                );
                assert_eq!(prepared.processed_nodes, 4 + parameter_count);
                for optional in [false, true] {
                    let all = candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        request.limits().max_syntax_nodes(),
                        &cancellation,
                    )
                    .unwrap();
                    let exact = all.len();
                    assert_eq!(
                        normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                            .unwrap()
                            .required,
                        required
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            request.source().bytes(),
                            optional,
                            exact,
                            &cancellation
                        )
                        .unwrap()
                        .len(),
                        exact
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            request.source().bytes(),
                            optional,
                            exact - 1,
                            &cancellation
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: exact,
                            limit: exact - 1,
                        })
                    );
                }
                fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes);
                let request = fixture.request(language, &[]);
                assert_eq!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    )
                    .unwrap()
                    .unwrap()
                    .processed_nodes,
                    prepared.processed_nodes
                );
                fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes - 1);
                let request = fixture.request(language, &[]);
                assert!(matches!(
                    prepare(&fixture.provider, &request, None, &[], fixture.provider.config.default_settings(), &cancellation),
                    Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                        resource: ResourceKind::SyntaxNodes,
                        observed,
                        limit,
                    })) if observed == prepared.processed_nodes && limit == prepared.processed_nodes - 1
                ));
                assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
                assert!(matches!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    ),
                    Err(AdapterError::Cancelled {
                        reason: rootlight_cancel::CancellationReason::ClientRequest,
                    })
                ));
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_scalar_contextual_names_match_the_private_native_route() {
        for body in [
            "Name = Name",
            "Let Name = Name",
            "Dim Name As Long\nName = Name",
            "Line = Line",
            "Let Line = Line",
            "Dim Line As Long\nLine = Line",
            "[Load] = 1",
        ] {
            let source = format!("Sub Run()\n{body}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(&source), language, &[]);
            }
        }
    }

    fn compare_native_procedural_body(body: &str) {
        let source = format!("Sub Run()\n{body}\nEnd Sub\n");
        for language in ["vba", "visual-basic-classic"] {
            compare_native(&Fixture::new(&source), language, &[]);
        }
    }

    #[test]
    fn first_party_vba_contextual_load_let_destination_matches_the_private_native_route() {
        compare_native_procedural_body("Let Load = 1");
    }

    #[test]
    fn first_party_vba_contextual_load_variable_declaration_matches_the_private_native_route() {
        compare_native_procedural_body("Dim Load As Long");
    }

    #[test]
    fn first_party_vba_contextual_load_constant_declaration_matches_the_private_native_route() {
        compare_native_procedural_body("Const Load As Long = 1");
    }

    #[test]
    fn first_party_vba_contextual_load_value_matches_the_private_native_route() {
        compare_native_procedural_body("value = Load");
    }

    #[test]
    fn first_party_vba_contextual_load_call_matches_the_private_native_route() {
        compare_native_procedural_body("Call Load()");
    }

    #[test]
    fn first_party_vba_contextual_unload_let_destination_matches_the_private_native_route() {
        compare_native_procedural_body("Let Unload = 1");
    }

    #[test]
    fn first_party_vba_contextual_unload_bracketed_assignment_matches_the_private_native_route() {
        compare_native_procedural_body("[Unload] = 1");
    }

    #[test]
    fn first_party_vba_contextual_bare_load_and_unload_keep_recovered_native_route() {
        for body in ["Load = 1", "lOaD = 1", "Unload = 1", "uNlOaD = 1"] {
            let source = format!("Sub Run()\n{body}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert!(native.traversal.fully_traversed);
                assert_eq!(
                    native.traversal.coverage,
                    CoverageStatus::Unknown,
                    "the native contextual destination requires recovery: {language}, {body}"
                );
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_none(),
                    "a recovered contextual destination must retain whole-module fallback: {language}, {body}"
                );
            }
        }
    }

    #[test]
    fn first_party_vba_statement_heads_never_gain_false_complete_admission() {
        let mut unexpected_admissions = Vec::new();
        for word in [
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
        ] {
            for (shape, body) in [
                ("assignment", format!("{word}=1")),
                ("let_assignment", format!("Let {word}=1")),
                ("variable_declaration", format!("Dim {word} As Long")),
                ("constant_declaration", format!("Const {word} As Long=1")),
                ("value", format!("value={word}")),
                ("call", format!("Call {word}()")),
            ] {
                let source = format!("Sub Run()\n{body}\nEnd Sub\n");
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let cancellation = Cancellation::new();
                    let native = fixture
                        .provider
                        .prepare_tree(
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap();
                    assert!(
                        native.traversal.fully_traversed,
                        "the finite head audit must finish traversal: {language}, {shape}, {word}"
                    );
                    let admitted = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_some();
                    if matches!(word, "Name" | "Line") && shape != "call" {
                        assert!(
                            admitted,
                            "a supported contextual name stays admitted: {language}, {shape}, {word}"
                        );
                    }
                    if native.traversal.coverage == CoverageStatus::Unknown {
                        if admitted {
                            unexpected_admissions.push(format!("{language}:{shape}:{word}"));
                        }
                    } else {
                        assert_eq!(
                            native.traversal.coverage,
                            CoverageStatus::Complete,
                            "the finite head oracle must not be resource-bounded: {language}, {shape}, {word}"
                        );
                        if admitted {
                            compare_native(&Fixture::new(&source), language, &[]);
                        }
                    }
                }
            }
        }
        assert!(
            unexpected_admissions.is_empty(),
            "native-recovered statement heads must retain whole-module fallback: {unexpected_admissions:?}"
        );
    }

    #[test]
    fn first_party_vba_zero_argument_calls_match_the_private_native_route() {
        let cases = [
            "Sub Run()\nCall Target\nEnd Sub\n",
            "Sub Run()\nCall Target()\nEnd Sub\n",
            "Sub Run()\nCall Target ()\nEnd Sub\n",
            "Sub Run()\ncAlL\tTarget ( \t )\nEnd Sub\n",
            "Sub Run()\nCall [Rem]\nCall [Me]()\nCall [Sub] ()\nCall [True]( )\nEnd Sub\n",
            "Sub Run()\nCall [Name]\nCall [Name]()\nCall [nAmE] ( )\nEnd Sub\n",
            "Sub Run()\nCall Target ' call:\nCall Target() ' parenthesized call\nEnd Sub\n",
            "Attribute VB_Name = \"ModuleA\"\r\nOption Explicit\r\nPrivate Sub Run() ' header\r\nCall Target() ' call\r\nCall [Rem] ' bare call\r\nEnd Sub ' terminator\r\n",
            "Sub Run():Call Target():Call [Rem]:End Sub\n",
            "Sub Run(): : Call Target(): ' call:\nCall Other\nEnd Sub\n",
            "Sub Run()\nCall _\r\n Target( _\n )\nCall Target _\n ( _\r\n )\nEnd Sub\n",
            "Sub Run()\nDim value As Long\nConst seed As Long = 7\nvalue = seed\nCall Target()\nLet value = 8\nCall [Name]\nEnd Sub\n",
            "Function Result() As Long\nCall Target\nResult = 7\nCall Target()\nEnd Function\n",
        ];
        for source in cases {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_contextual_name_keeps_recovered_native_route() {
        for call in ["Call Name", "Call Name()", "Call nAmE ( )"] {
            let source = format!("Sub Run()\n{call}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_none(),
                    "a recovered contextual callee must retain whole-module fallback: {language}, {call}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert!(native.traversal.fully_traversed);
                assert_eq!(
                    native.traversal.coverage,
                    CoverageStatus::Unknown,
                    "the native contextual callee requires recovery: {language}, {call}"
                );
            }
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_contextual_line_matches_the_private_native_route() {
        for language in ["vba", "visual-basic-classic"] {
            compare_native(
                &Fixture::new("Sub Run()\nCall Line\nEnd Sub\n"),
                language,
                &[],
            );
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_underscore_name_matches_the_private_native_route() {
        for language in ["vba", "visual-basic-classic"] {
            compare_native(
                &Fixture::new("Sub Run()\nCall _Target()\nEnd Sub\n"),
                language,
                &[],
            );
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_boundaries_keep_whole_native_fallback() {
        for call in [
            "Call Target(1)",
            "Call Target value",
            "Call Target(value:=1)",
            "Call Target(,)",
            "Call Module.Target()",
            "Call .Target()",
            "Call Target() extra",
            "Call Target()()",
            "Call Target(",
            "Call Target)",
            "Call Target(())",
            "Call[Target]",
            "Call_Target",
            "Call\x0cTarget()",
            "Call\nTarget()",
            "Call Target$()",
            "Call Me()",
            "Call Rem()",
            "Call True()",
        ] {
            let source = format!("Sub Run()\nDim value As Long\nvalue = 7\n{call}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "an unsupported call tail retains whole-module native parsing: {language}, {call}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_candidate_ceilings_and_cancellation_are_exact() {
        let source = "Sub Run()\nDim value As Long\nCall Target()\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source);
            let request = fixture.request(language, &[]);
            let cancellation = Cancellation::new();
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            let identity_candidates = candidates(
                &prepared.inventory,
                request.source().bytes(),
                false,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap();
            assert!(
                identity_candidates
                    .iter()
                    .all(|candidate| candidate.role != StructuralRole::Reference)
            );
            let identity = normalize_query_candidates(
                identity_candidates,
                &request,
                usize::MAX,
                &cancellation,
            )
            .unwrap();
            let full_candidates = candidates(
                &prepared.inventory,
                request.source().bytes(),
                true,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap();
            let references: Vec<_> = full_candidates
                .iter()
                .filter(|candidate| candidate.role == StructuralRole::Reference)
                .map(|candidate| {
                    (
                        source.get(candidate.start..candidate.end).unwrap(),
                        candidate.syntax,
                        candidate.required,
                    )
                })
                .collect();
            assert_eq!(
                references,
                [
                    ("Call Target()", "vba.application", false),
                    ("Target", "vba.invoked_identifier", false),
                ]
            );
            let full =
                normalize_query_candidates(full_candidates, &request, usize::MAX, &cancellation)
                    .unwrap();
            assert_eq!(identity.required, full.required);
            for optional in [false, true] {
                let exact = candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    request.limits().max_syntax_nodes(),
                    &cancellation,
                )
                .unwrap()
                .len();
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        exact,
                        &cancellation,
                    )
                    .unwrap()
                    .len(),
                    exact
                );
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        exact - 1,
                        &cancellation,
                    )
                    .unwrap_err(),
                    AdapterError::RejectedRequest(RequestError::ProviderLimit {
                        resource: ResourceKind::SyntaxNodes,
                        observed: exact,
                        limit: exact - 1,
                    })
                );
            }
            assert_eq!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    false,
                    0,
                    &cancellation,
                )
                .unwrap_err(),
                AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 9,
                    limit: 0,
                })
            );
            assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    true,
                    request.limits().max_syntax_nodes(),
                    &cancellation,
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest,
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_scalar_body_boundaries_keep_whole_native_fallback() {
        for body in [
            "Dim value As Long\nvalue = 1 + 2",
            "Dim value As Long\nCall Run(1)",
            "Dim value As Long\nRun value",
            "Dim value As Long\nholder.Value = 1",
            "Dim values(1) As Long",
            "Dim first As Long, second As Long",
            "Dim value",
            "Dim value As UserType",
            "Const seed As Long = value",
            "Dim value As Long\nvalue = -1",
            "Dim value As Long\nvalue = 1.5",
            "Dim value As Long\nvalue = &H10",
            "Dim value As Long\nvalue = Nothing",
            "Dim value As Long\nvalue = Me",
            "Dim value As Long\nMe = value",
            "Dim value As Long\nIf True Then\nvalue = 1\nEnd If",
            "Dim value As Long\nvalue = \"unterminated _\n",
        ] {
            let source = format!("Sub Run()\n{body}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "the entire unsupported body retains native parsing: {language}, {body}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_scalar_candidate_ceilings_and_cancellation_are_exact() {
        let fixture = Fixture::new(
            "Sub Run()\nDim value As Long\nConst seed As Long = 7\nvalue = seed\nEnd Sub\n",
        );
        let request = fixture.request("vba", &[]);
        let cancellation = Cancellation::new();
        let prepared = prepare(
            &fixture.provider,
            &request,
            None,
            &[],
            fixture.provider.config.default_settings(),
            &cancellation,
        )
        .unwrap()
        .unwrap();
        for optional in [false, true] {
            let exact = candidates(
                &prepared.inventory,
                request.source().bytes(),
                optional,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap()
            .len();
            assert!(exact > 1);
            assert_eq!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    exact,
                    &cancellation,
                )
                .unwrap()
                .len(),
                exact
            );
            assert_eq!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    exact - 1,
                    &cancellation,
                )
                .unwrap_err(),
                AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: exact,
                    limit: exact - 1,
                })
            );
        }
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            candidates(
                &prepared.inventory,
                request.source().bytes(),
                true,
                request.limits().max_syntax_nodes(),
                &cancellation,
            ),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest,
            })
        ));
        assert_eq!(fixture.provider.stats().pooled_parsers, 0);
        assert_eq!(fixture.provider.stats().cache.entries, 0);
    }

    #[test]
    fn first_party_vba_admitted_ascii_edges_match_the_private_native_route() {
        const MODULE: &str = "Sub Run()\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut admitted = 0_usize;
            for byte in 0_u8..=127 {
                let edge = char::from(byte);
                for source in [format!("{edge}{MODULE}"), format!("{MODULE}{edge}")] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    // Unsupported input retains the native route; only complete admission
                    // creates a first-party/native equivalence obligation.
                    if prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_some()
                    {
                        admitted += 1;
                        compare_native(&fixture, language, &[]);
                    }
                }
            }
            assert!(admitted > 0, "the ASCII matrix must exercise admission");
        }
    }

    #[test]
    fn first_party_vba_admitted_names_and_suffixes_match_the_private_native_route() {
        let mut sources = Vec::new();
        for name in [
            "Run",
            "_",
            "_Run",
            "Rem",
            "rem",
            "rEm",
            "Remnant",
            "Reminder",
            "Sub",
            "Function",
            "End",
            "Name",
            "Public",
            "Option",
            "String",
            "True",
            "Nothing",
            "[Rem]",
            "[Sub]",
            "[Function]",
            "[End]",
            "[Name]",
            "[_]",
        ] {
            for kind in ["Sub", "Function"] {
                for parameters in ["", "()", " ()"] {
                    sources.push(format!("{kind} {name}{parameters}\nEnd {kind}\n"));
                }
            }
        }
        for name in ["Result", "Rem"] {
            for suffix in ['%', '&', '^', '!', '#', '@', '$'] {
                sources.push(format!("Function {name}{suffix}()\nEnd Function\n"));
            }
        }
        for language in ["vba", "visual-basic-classic"] {
            let mut admitted = 0_usize;
            for source in &sources {
                let fixture = Fixture::new(source);
                let request = fixture.request(language, &[]);
                if prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &Cancellation::new(),
                )
                .unwrap()
                .is_some()
                {
                    admitted += 1;
                    compare_native(&fixture, language, &[]);
                }
            }
            assert!(
                admitted >= 28,
                "the name and suffix matrix must exercise admission"
            );
        }
    }

    #[test]
    fn first_party_vba_recovered_module_syntax_keeps_the_native_route() {
        for source in [
            "Option Explicit:Sub Run()\nEnd Sub\n",
            "Attribute VB_Name = \"ModuleA\":Sub Run()\nEnd Sub\n",
            "Sub Run()\nEnd Sub:",
            "Sub Rem\nEnd Sub\n",
            "Sub Rem()\nEnd Sub\n",
            "Sub Rem ()\nEnd Sub\n",
            "Function Rem()\nEnd Function\n",
            "Function Rem!()\nEnd Function\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert_eq!(
                    native.traversal.coverage,
                    CoverageStatus::Unknown,
                    "the authored boundary requires native recovery: {source}"
                );
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_none(),
                    "recovered native source must not gain complete admission: {source}"
                );
            }
        }
    }

    #[test]
    fn first_party_vba_included_ranges_fall_back_with_exact_native_identity_closure() {
        let source =
            "Attribute VB_Name = \"ModuleA\"\nSub First()\nEnd Sub\nSub Second()\nEnd Sub\n";
        let start = u64::try_from(source.find("Sub Second").unwrap()).unwrap();
        let end = u64::try_from(source.trim_end().len()).unwrap();
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source);
            let cancellation = Cancellation::with_deadline(
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            );
            let request = fixture.request(language, &[(start, end)]);
            assert!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                )
                .unwrap()
                .is_none()
            );
            let native = fixture
                .provider
                .prepare_tree(
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap();
            assert!(native.traversal.fully_traversed);
            assert_eq!(native.traversal.coverage, CoverageStatus::Bounded);
            let pack = fixture
                .provider
                .query_packs
                .get_for_source(native.family, request.source().path().as_str())
                .unwrap();
            let legacy = pack
                .extract(
                    native.family,
                    &native.tree,
                    request.source().bytes(),
                    request.limits().max_syntax_nodes(),
                    usize::MAX,
                    &cancellation,
                )
                .unwrap();
            assert!(legacy.limit.is_none());
            let expected =
                normalize_query_candidates(legacy.candidates, &request, usize::MAX, &cancellation)
                    .unwrap();
            let expected_identity = normalize_query_candidates(
                pack.extract_identity(
                    native.family,
                    &native.tree,
                    request.source().bytes(),
                    request.limits().max_syntax_nodes(),
                    &cancellation,
                )
                .unwrap(),
                &request,
                usize::MAX,
                &cancellation,
            )
            .unwrap();
            assert_eq!(
                fixture
                    .provider
                    .required_syntax_fact_count(&request, &cancellation)
                    .unwrap(),
                expected_identity.required
            );
            let output = rootlight_adapter_sdk::execute_parse(
                &fixture.provider,
                &request,
                rootlight_adapter_sdk::MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            )
            .unwrap();
            assert_eq!(output.facts(), expected.facts);
            assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        }
    }

    #[test]
    fn first_party_vba_never_accepts_a_supported_prefix_or_object_context() {
        for source in [
            "Sub Run()\nDim value As Long\nvalue = 1 + 2\nEnd Sub\n",
            "Sub Run()\nEnd Sub\n#If Host Then\n#End If\n",
        ] {
            let fixture = Fixture::new(source);
            let request = fixture.request("vba", &[]);
            assert!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &Cancellation::new()
                )
                .unwrap()
                .is_none()
            );
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
        }
        let fixture = Fixture::new("Attribute VB_Name = \"ModuleA\"\nSub Run()\nEnd Sub\n");
        let analysis = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            LanguageId::new("vba").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(fixture.source.content_hash()),
            &fixture.limits,
        )
        .unwrap()
        .with_native_vba_module(NativeVbaModuleKind::Object)
        .unwrap();
        let request = analysis.to_parse_request();
        assert!(
            prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
    }
}
