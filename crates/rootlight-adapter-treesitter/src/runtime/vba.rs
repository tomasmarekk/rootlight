//! Closed first-party procedural modules enter the existing syntax normalizer.
//! Unsupported source returns to the native whole-module path; resource failures
//! never trigger a second parser, and successful scans retain no native tree.

use std::ops::Range;

use rootlight_adapter_sdk::NativeVbaModuleKind;
use rootlight_vba::{ModuleError, ModuleInventory, ModuleLimits, ProcedureKind};

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
    let limits = ModuleLimits::new(maximum_source, maximum_nodes, maximum_nodes);
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
            Err(ModuleError::StatementLimit | ModuleError::ProcedureLimit) => {
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
    let processed_nodes = inventory
        .statement_count()
        .checked_add(inventory.procedures().len())
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
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
    let count = inventory
        .procedures()
        .len()
        .checked_mul(4)
        .and_then(|count| count.checked_add(3 + usize::from(inventory.module_name().is_some())))
        .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
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
    }
    if include_optional {
        lexical_trivia(source, &mut output, maximum, cancellation)?;
    }
    cancellation.check()?;
    Ok(output)
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
        let body_bytes = source
            .get(body.clone())
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
        .expect("the authored complete empty-body module is supported");
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
            actual.facts, expected.facts,
            "exact source facts for {language}"
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
            "Sub Run()\nunknown = 1\nEnd Sub\n",
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
