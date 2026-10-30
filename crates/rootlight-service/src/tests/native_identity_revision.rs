//! Semantic provider changes must invalidate caches even with the same grammar.
//! Predecessor recipes hold every other input fixed so grammar or discovery
//! changes cannot conceal a missing analyzer, resolver or project identity bump.

use super::*;

#[test]
fn bounded_native_batches_invalidate_previous_file_only_generations() {
    let parser = first_slice_parser_provider_hash().unwrap();
    let previous = hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        DISCOVERY_MANIFEST_VERSION.as_bytes(),
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        b"rootlight.source-file-fallback/5",
        container_sources::REVISION,
    ])
    .unwrap();
    assert_eq!(
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        b"rootlight.source-file-fallback/6"
    );
    assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
}

#[test]
fn bounded_xml_depth_proofs_invalidate_the_previous_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/361",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn xliff_companions_invalidate_previous_analyzer_and_discovery_identities() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/360",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_ne!(parser, previous_parser);
    let previous_discovery = hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        b"1.90",
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        container_sources::REVISION,
    ])
    .unwrap();
    assert_eq!(DISCOVERY_MANIFEST_VERSION, "1.91");
    assert_ne!(first_slice_provider_set_hash().unwrap(), previous_discovery);
}

#[test]
fn typespec_native_support_invalidates_the_previous_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/359",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn xml_encoding_admission_invalidates_the_previous_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/358",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn unversioned_pwct_models_invalidate_the_previous_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/357",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn native_analyzer_and_resolver_predecessors_invalidate_same_grammar_cache() {
    let cancellation = Cancellation::new();
    let limits = PlanningLimits::default();
    let repository = RepositoryId::from_bytes([1; 16]);
    let file = FileId::from_bytes([2; 20]);
    let provider = derive_fact("incremental-provider", INCREMENTAL_PROVIDER_SEED).id();
    let adapter_key = InputKey::AdapterVersion(provider);
    let grammar_key = InputKey::GrammarVersion(
        derive_fact("rootlight.first-slice.grammar-input", GRAMMAR_REVISION_SEED).id(),
    );
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let handlebars_grammar = |predecessor: Option<(&str, &str)>| {
        let registry = GrammarRegistry::audited().unwrap();
        let mut hasher = blake3::Hasher::new();
        hash_component(&mut hasher, GRAMMAR_REVISION_SEED).unwrap();
        hash_component(&mut hasher, TREE_SITTER_RUNTIME_VERSION.as_bytes()).unwrap();
        for descriptor in registry.descriptors() {
            hash_component(&mut hasher, descriptor.language().as_str().as_bytes()).unwrap();
            hash_component(&mut hasher, descriptor.grammar_version().as_bytes()).unwrap();
            let old = predecessor.filter(|_| descriptor.language().as_str() == "handlebars");
            let source = old.map_or(descriptor.grammar_source_sha256(), |(source, _)| source);
            let parser = old.map_or(descriptor.parser_sha256(), |(_, parser)| parser);
            hash_component(&mut hasher, source.as_bytes()).unwrap();
            hash_component(&mut hasher, parser.as_bytes()).unwrap();
            match descriptor.scanner_sha256() {
                Some(scanner) => {
                    hash_component(&mut hasher, b"scanner").unwrap();
                    hash_component(&mut hasher, scanner.as_bytes()).unwrap();
                }
                None => hash_component(&mut hasher, b"no-scanner").unwrap(),
            }
            hash_component(
                &mut hasher,
                &u64::try_from(descriptor.abi_version())
                    .unwrap()
                    .to_be_bytes(),
            )
            .unwrap();
        }
        ContentHash::from_bytes(*hasher.finalize().as_bytes())
    };
    assert_eq!(handlebars_grammar(None), grammar);
    let predecessor_escaped_grammar = handlebars_grammar(Some((
        "73e92995115833b45caaf919f30b293c6f41c64dbb1593e3548cda983082ab79",
        "ca3c5080a4b1771bcd36738b7ec525e2af8197b5c63321e60fed8434c5e8a92a",
    )));
    assert_ne!(predecessor_escaped_grammar, grammar);
    let predecessor_escaped_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        ANALYZER_BINARY_SEED,
        predecessor_escaped_grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_parent_data_grammar = handlebars_grammar(Some((
        "66550dc98880d297b2ee7de9e90528c5ce38ed1906f703d4c9fb02d5cd2d0805",
        "e673a66615c1837468316407d2ddc9d250b3ae48371e68e8dcb2496f52118be9",
    )));
    assert_ne!(predecessor_parent_data_grammar, grammar);
    let predecessor_parent_data_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        ANALYZER_BINARY_SEED,
        predecessor_parent_data_grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_triple_block_grammar = handlebars_grammar(Some((
        "d0fc746399aaaa9476a76e8873ff2da190d23bce0d7b6880933343f8f87215a2",
        "2419bda4ca199228bc47a1ad621585578e9a98b70b7e795f678c9012a0aa05db",
    )));
    assert_ne!(predecessor_triple_block_grammar, grammar);
    let predecessor_triple_block_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        ANALYZER_BINARY_SEED,
        predecessor_triple_block_grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/294",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_const_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/330",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_cuda_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/333",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_eiffel_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/332",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_bash_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/331",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_kotlin_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/345",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_kotlin_formal_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/346",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_cpp_signature_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/349",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_javascript_formal_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/350",
        grammar.as_bytes(),
    ])
    .unwrap();
    let predecessor_handlebars_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/351",
        grammar.as_bytes(),
    ])
    .unwrap();
    let current = InputSnapshot::new(
        [
            InputFingerprint::new(
                InputKey::FileContent(file),
                content_hash(b"unchanged-content"),
            ),
            InputFingerprint::new(InputKey::FilePath(file), content_hash(b"unchanged-path")),
            InputFingerprint::new(
                InputKey::FileParserRoute(file),
                content_hash(b"unchanged-route"),
            ),
            InputFingerprint::new(grammar_key, grammar),
            InputFingerprint::new(adapter_key, first_slice_parser_provider_hash().unwrap()),
            InputFingerprint::new(
                InputKey::ResolverVersion,
                content_hash(RESOLVER_BINARY_SEED),
            ),
            InputFingerprint::new(
                InputKey::ConfigurationRevision,
                content_hash(b"unchanged-configuration"),
            ),
            InputFingerprint::new(InputKey::SearchRevision, content_hash(SEARCH_REVISION_SEED)),
        ],
        limits,
        &cancellation,
    )
    .unwrap();
    let keys = incremental_input_keys(&current, &current, &cancellation).unwrap();
    let passes = first_slice_passes().unwrap();
    let (nodes, edges) = first_slice_dependency_parts(
        repository,
        &BTreeSet::from([file]),
        &keys,
        &passes,
        &cancellation,
    )
    .unwrap();
    let graph_limits = GraphLimits::new(5, nodes.len(), edges.len()).unwrap();
    let registry =
        DependencyRegistry::new(passes.declarations().unwrap(), graph_limits, &cancellation)
            .unwrap();
    verify_first_slice_observations(&registry, &passes).unwrap();
    let graph = DependencyGraph::new(nodes, edges, &registry, graph_limits, &cancellation).unwrap();
    let artifact_id = parser_artifact_id(file);
    let summary = |inputs: InputSnapshot| {
        let artifact = ArtifactSummary::new(
            artifact_id,
            [FactNode::new(file_analysis_unit(file), FactDomain::Syntax)],
            inputs
                .iter()
                .filter(|input| parser_artifact_depends_on(input.key(), file)),
            limits,
            &cancellation,
        )
        .unwrap();
        GenerationSummary::new(inputs, [artifact], limits, &cancellation).unwrap()
    };
    let unchanged = summary(current.clone());
    let reuse = plan_invalidation(&unchanged, &current, &graph, limits, &cancellation).unwrap();
    assert!(reuse.changes().changes().is_empty());
    assert_eq!(reuse.invalidated_nodes().count(), 0);
    assert_eq!(reuse.artifact_decisions().len(), 1);
    assert_eq!(
        reuse.artifact_decisions()[0].kind(),
        ArtifactDecisionKind::Reuse
    );

    for (changed_key, predecessor, expected) in [
        (
            grammar_key,
            predecessor_triple_block_grammar,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_triple_block_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            grammar_key,
            predecessor_parent_data_grammar,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_parent_data_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            grammar_key,
            predecessor_escaped_grammar,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_escaped_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_handlebars_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_javascript_formal_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_cpp_signature_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_kotlin_formal_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_kotlin_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_cuda_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            InputKey::ResolverVersion,
            content_hash(b"rootlight.first-slice.resolve/143"),
            ArtifactDecisionKind::Reuse,
        ),
        (
            adapter_key,
            predecessor_eiffel_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            predecessor_bash_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            InputKey::ResolverVersion,
            content_hash(b"rootlight.first-slice.resolve/142"),
            ArtifactDecisionKind::Reuse,
        ),
        (
            adapter_key,
            predecessor_const_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            InputKey::ResolverVersion,
            content_hash(b"rootlight.first-slice.resolve/141"),
            ArtifactDecisionKind::Reuse,
        ),
        (
            adapter_key,
            predecessor_parser,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            InputKey::ResolverVersion,
            content_hash(b"rootlight.first-slice.resolve/125"),
            ArtifactDecisionKind::Reuse,
        ),
        (
            InputKey::ResolverVersion,
            content_hash(b"rootlight.first-slice.resolve/140"),
            ArtifactDecisionKind::Reuse,
        ),
    ] {
        let previous = InputSnapshot::new(
            current.iter().map(|input| {
                if input.key() == changed_key {
                    InputFingerprint::new(changed_key, predecessor)
                } else {
                    input
                }
            }),
            limits,
            &cancellation,
        )
        .unwrap();
        if changed_key == grammar_key {
            assert!(
                [
                    predecessor_escaped_grammar,
                    predecessor_parent_data_grammar,
                    predecessor_triple_block_grammar,
                ]
                .contains(&predecessor)
            );
            assert_eq!(
                previous.iter().find(|input| input.key() == grammar_key),
                Some(InputFingerprint::new(grammar_key, predecessor))
            );
        } else {
            assert_eq!(
                previous.iter().find(|input| input.key() == grammar_key),
                current.iter().find(|input| input.key() == grammar_key)
            );
        }
        for input in current.iter().filter(|input| input.key() != changed_key) {
            assert_eq!(
                previous.iter().find(|value| value.key() == input.key()),
                Some(input)
            );
        }
        let parent = summary(previous);
        let plan = plan_invalidation(&parent, &current, &graph, limits, &cancellation).unwrap();
        assert_eq!(
            plan.changes()
                .changes()
                .iter()
                .map(|change| change.key())
                .collect::<Vec<_>>(),
            vec![changed_key]
        );
        assert!(plan.fallback().is_none());
        assert_eq!(plan.artifact_decisions().len(), 1);
        assert_eq!(plan.artifact_decisions()[0].artifact(), artifact_id);
        assert_eq!(plan.artifact_decisions()[0].kind(), expected);
        if changed_key == adapter_key || changed_key == grammar_key {
            assert!(plan.invalidated_nodes().any(|node|
                node == FactNode::new(file_analysis_unit(file), FactDomain::Syntax)));
        } else {
            assert_eq!(
                plan.invalidated_nodes()
                    .map(|node| node.domain())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([
                    FactDomain::Resolution,
                    FactDomain::DerivedGraph,
                    FactDomain::Search
                ])
            );
        }
    }

    // Classification changes invalidate generation identity independently of
    // analyzer/resolver migrations, which use the current values on both sides.
    let parser = first_slice_parser_provider_hash().unwrap();
    let previous_discovery = hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        b"1.64",
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        container_sources::REVISION,
    ])
    .unwrap();
    assert_ne!(first_slice_provider_set_hash().unwrap(), previous_discovery);
    let prior_handlebars_discovery = hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        b"1.80",
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        container_sources::REVISION,
    ])
    .unwrap();
    assert_ne!(
        first_slice_provider_set_hash().unwrap(),
        prior_handlebars_discovery
    );
}

#[test]
fn project_predecessor_cannot_reuse_deep_provider_contexts() {
    let unavailable = FirstSliceService::new(2).unwrap();
    let project_identity = content_hash(b"unchanged-project-analyzer");
    let configured = FirstSliceService::new_with_storage(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        Some(Arc::new(FailingProjectAnalyzer {
            identity: project_identity,
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::new(AtomicUsize::new(0)),
        })),
    )
    .unwrap();
    let contexts: [(&FirstSliceService, &[u8]); 2] = [
        (&unavailable, b"unavailable"),
        (&configured, project_identity.as_bytes()),
    ];
    for (service, project) in contexts {
        let structural = service
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap();
        let predecessor = hash_static_components(&[
            b"rootlight.first-slice.project-provider/18",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        let current = service
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap();
        assert_ne!(current, predecessor);
        let preceding_trait_candidates = hash_static_components(&[
            b"rootlight.first-slice.project-provider/24",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        assert_ne!(current, preceding_trait_candidates);
        let preceding_package_functions = hash_static_components(&[
            b"rootlight.first-slice.project-provider/25",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        assert_ne!(current, preceding_package_functions);
        let preceding_test_imports = hash_static_components(&[
            b"rootlight.first-slice.project-provider/26",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        assert_ne!(current, preceding_test_imports);
        let preceding_php_method_proofs = hash_static_components(&[
            b"rootlight.first-slice.project-provider/27",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        assert_ne!(current, preceding_php_method_proofs);
        let preceding_php_ordinary_proofs = hash_static_components(&[
            b"rootlight.first-slice.project-provider/28",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        assert_ne!(current, preceding_php_ordinary_proofs);
        assert_ne!(current, structural);
    }
    assert_eq!(
        unavailable
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap(),
        configured
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap()
    );
    assert_ne!(
        unavailable
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap(),
        configured
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap()
    );
}

// These controls use artifacts produced by the admitted Docker and Coffee
// frontends, then change only one predecessor fingerprint with grammar fixed.

fn native296_written_sources() -> (
    TempDir,
    FirstSliceService,
    FirstSliceIndexReceipt,
    BTreeSet<FileId>,
) {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let docker = b"FROM scratch AS written\n";
    let coffee = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/coffeescript/line.coffee"
    );
    assert_eq!(coffee.len(), 467);
    fs::write(root.path().join("Dockerfile"), docker).unwrap();
    fs::write(root.path().join("written.coffee"), coffee).unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(3).unwrap();
    let receipt = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(receipt.indexed_files, 2);
    let mut files = BTreeSet::new();
    {
        let lease = service
            .generation_lease(receipt.generation, &cancellation)
            .unwrap();
        let document = lease.generation().document();
        for (path, language, kind, name, bytes) in [
            (
                "Dockerfile",
                "dockerfile",
                EntityKind::BuildTarget,
                "written",
                docker.as_slice(),
            ),
            (
                "written.coffee",
                "coffeescript",
                EntityKind::Class,
                "Line",
                coffee.as_slice(),
            ),
        ] {
            let file = document
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap();
            assert_eq!(file.language, language);
            let source = file.evidence.source.as_ref().unwrap();
            assert_eq!(source.content_hash(), content_hash(bytes));
            let entities = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == kind
                        && entity.canonical_name == name
                        && entity.evidence.source.as_ref().is_some_and(|site| {
                            site.span().file() == file.id
                                && site.content_hash() == content_hash(bytes)
                        })
                })
                .collect::<Vec<_>>();
            assert_eq!(entities.len(), 1, "actual native identity for {path}");
            assert!(document.occurrences.iter().any(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().file() == file.id
                    && site.source.content_hash() == content_hash(bytes)
                    && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == entities[0].id)
            }));
            files.insert(file.id);
        }
    }
    assert_eq!(
        service
            .index_repository(root.path(), &cancellation)
            .unwrap(),
        receipt
    );
    let retained = service
        .structural_artifacts
        .generation(receipt.generation)
        .unwrap();
    assert_eq!(retained.len(), 2);
    assert_eq!(
        retained
            .iter()
            .map(|(file, _)| *file)
            .collect::<BTreeSet<_>>(),
        files
    );
    (root, service, receipt, files)
}

#[test]
fn native296_same_grammar_artifacts_rebuild_for_295_and_reuse_for_resolver_126() {
    let (_root, service, receipt, files) = native296_written_sources();
    let cancellation = Cancellation::new();
    let limits = PlanningLimits::default();
    let current = service.incremental_inputs.get(&receipt.generation).unwrap();
    let retained = service
        .structural_artifacts
        .generation(receipt.generation)
        .unwrap();
    let provider = derive_fact("incremental-provider", INCREMENTAL_PROVIDER_SEED).id();
    let adapter_key = InputKey::AdapterVersion(provider);
    let grammar_key = InputKey::GrammarVersion(
        derive_fact("rootlight.first-slice.grammar-input", GRAMMAR_REVISION_SEED).id(),
    );
    let grammar = first_slice_grammar_revision_hash().unwrap();
    assert_eq!(current.value(grammar_key), Some(grammar));
    assert_eq!(
        current.value(adapter_key),
        Some(first_slice_parser_provider_hash().unwrap())
    );
    let keys = incremental_input_keys(current, current, &cancellation).unwrap();
    let passes = first_slice_passes().unwrap();
    let (nodes, edges) =
        first_slice_dependency_parts(receipt.repository, &files, &keys, &passes, &cancellation)
            .unwrap();
    let graph_limits = GraphLimits::new(5, nodes.len(), edges.len()).unwrap();
    let registry =
        DependencyRegistry::new(passes.declarations().unwrap(), graph_limits, &cancellation)
            .unwrap();
    verify_first_slice_observations(&registry, &passes).unwrap();
    let graph = DependencyGraph::new(nodes, edges, &registry, graph_limits, &cancellation).unwrap();
    let summary = |inputs: InputSnapshot| {
        let artifacts =
            parent_artifact_summaries(&inputs, Some(retained), limits, &cancellation).unwrap();
        assert_eq!(artifacts.len(), 2);
        GenerationSummary::new(inputs, artifacts, limits, &cancellation).unwrap()
    };
    let unchanged = summary(current.clone());
    let reuse = plan_invalidation(&unchanged, current, &graph, limits, &cancellation).unwrap();
    assert!(reuse.changes().changes().is_empty());
    assert_eq!(reuse.invalidated_nodes().count(), 0);
    assert_eq!(reuse.artifact_decisions().len(), 2);
    assert!(
        reuse
            .artifact_decisions()
            .iter()
            .all(|decision| decision.kind() == ArtifactDecisionKind::Reuse)
    );
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/295",
        grammar.as_bytes(),
    ])
    .unwrap();
    let host_only_353 = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/353",
        grammar.as_bytes(),
    ])
    .unwrap();
    let syntax_only_354 = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/354",
        grammar.as_bytes(),
    ])
    .unwrap();
    let before_markdown_host_certificate = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/355",
        grammar.as_bytes(),
    ])
    .unwrap();
    let before_fenced_handlebars_attribution = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/356",
        grammar.as_bytes(),
    ])
    .unwrap();
    for (changed_key, previous, expected) in [
        (adapter_key, previous_parser, ArtifactDecisionKind::Rebuild),
        (adapter_key, host_only_353, ArtifactDecisionKind::Rebuild),
        (adapter_key, syntax_only_354, ArtifactDecisionKind::Rebuild),
        (
            adapter_key,
            before_markdown_host_certificate,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            adapter_key,
            before_fenced_handlebars_attribution,
            ArtifactDecisionKind::Rebuild,
        ),
        (
            InputKey::ResolverVersion,
            content_hash(b"rootlight.first-slice.resolve/126"),
            ArtifactDecisionKind::Reuse,
        ),
    ] {
        let previous = InputSnapshot::new(
            current.iter().map(|input| {
                if input.key() == changed_key {
                    InputFingerprint::new(changed_key, previous)
                } else {
                    input
                }
            }),
            limits,
            &cancellation,
        )
        .unwrap();
        assert_eq!(previous.value(grammar_key), current.value(grammar_key));
        let parent = summary(previous);
        let plan = plan_invalidation(&parent, current, &graph, limits, &cancellation).unwrap();
        assert_eq!(
            plan.changes()
                .changes()
                .iter()
                .map(|change| change.key())
                .collect::<Vec<_>>(),
            vec![changed_key]
        );
        assert!(plan.fallback().is_none());
        assert_eq!(plan.artifact_decisions().len(), 2);
        for file in &files {
            let decision = plan
                .artifact_decisions()
                .iter()
                .find(|decision| decision.artifact() == parser_artifact_id(*file))
                .unwrap();
            assert_eq!(decision.kind(), expected);
            if changed_key == adapter_key {
                assert!(plan.invalidated_nodes().any(
                    |node| node == FactNode::new(file_analysis_unit(*file), FactDomain::Syntax)
                ));
            }
        }
        if changed_key == InputKey::ResolverVersion {
            assert_eq!(
                plan.invalidated_nodes()
                    .map(|node| node.domain())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([
                    FactDomain::Resolution,
                    FactDomain::DerivedGraph,
                    FactDomain::Search
                ])
            );
        }
    }
}

#[test]
fn native296_project_19_cannot_reuse_either_deep_provider_context() {
    let unavailable = FirstSliceService::new(2).unwrap();
    let identity = content_hash(b"unchanged-project-analyzer");
    let configured = FirstSliceService::new_with_storage(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        Some(Arc::new(FailingProjectAnalyzer {
            identity,
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::new(AtomicUsize::new(0)),
        })),
    )
    .unwrap();
    for (service, project) in [
        (&unavailable, b"unavailable".as_slice()),
        (&configured, identity.as_bytes().as_slice()),
    ] {
        let structural = service
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap();
        let previous = hash_static_components(&[
            b"rootlight.first-slice.project-provider/19",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        let current = service
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap();
        assert_ne!(current, previous);
        assert_ne!(current, structural);
    }
    assert_eq!(
        unavailable
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap(),
        configured
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap()
    );
    assert_ne!(
        unavailable
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap(),
        configured
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap()
    );
}

#[test]
fn native296_discovery_165_cannot_reuse_written_language_generation_context() {
    let (_root, service, _receipt, _files) = native296_written_sources();
    let parser = first_slice_parser_provider_hash().unwrap();
    let previous = hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        b"1.65",
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        container_sources::REVISION,
    ])
    .unwrap();
    assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    let policy = crate::aggregate_source_recovery::policy_hash(
        service.analysis_limits.ir(),
        service.generation_budget,
    )
    .unwrap();
    let previous = hash_static_components(&[previous.as_bytes(), policy.as_bytes()]).unwrap();
    assert_ne!(
        service
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap(),
        previous
    );
}

fn written_field_296_parser_hash() -> ContentHash {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/296",
        grammar.as_bytes(),
    ])
    .unwrap()
}

#[test]
fn written_calls_cannot_reuse_previous_parser_resolver_or_project_context() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/297",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous_parser);
    let previous_provider = hash_static_components(&[
        PROVIDER_SET_SEED,
        previous_parser.as_bytes(),
        DISCOVERY_MANIFEST_VERSION.as_bytes(),
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        b"rootlight.first-slice.resolve/127",
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        container_sources::REVISION,
    ])
    .unwrap();
    assert_ne!(first_slice_provider_set_hash().unwrap(), previous_provider);

    let unavailable = FirstSliceService::new(2).unwrap();
    let identity = content_hash(b"unchanged-go-project-analyzer");
    let configured = FirstSliceService::new_with_storage(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        Some(Arc::new(FailingProjectAnalyzer {
            identity,
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::new(AtomicUsize::new(0)),
        })),
    )
    .unwrap();
    for (service, project) in [
        (&unavailable, b"unavailable".as_slice()),
        (&configured, identity.as_bytes().as_slice()),
    ] {
        let structural = service
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap();
        let previous = hash_static_components(&[
            b"rootlight.first-slice.project-provider/20",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        assert_ne!(
            service
                .provider_set_hash(FirstSliceIndexMode::Deep)
                .unwrap(),
            previous
        );
    }
}

#[test]
fn written_fields_same_grammar_296_requires_structural_artifact_rebuild() {
    let cancellation = Cancellation::new();
    let limits = PlanningLimits::default();
    let repository = RepositoryId::from_bytes([1; 16]);
    let file = FileId::from_bytes([2; 20]);
    let provider = derive_fact("incremental-provider", INCREMENTAL_PROVIDER_SEED).id();
    let adapter_key = InputKey::AdapterVersion(provider);
    let grammar_key = InputKey::GrammarVersion(
        derive_fact("rootlight.first-slice.grammar-input", GRAMMAR_REVISION_SEED).id(),
    );
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let current = InputSnapshot::new(
        [
            InputFingerprint::new(
                InputKey::FileContent(file),
                content_hash(b"unchanged-written-field-source"),
            ),
            InputFingerprint::new(InputKey::FilePath(file), content_hash(b"unchanged-path")),
            InputFingerprint::new(
                InputKey::FileParserRoute(file),
                content_hash(b"unchanged-csharp-route"),
            ),
            InputFingerprint::new(grammar_key, grammar),
            InputFingerprint::new(adapter_key, first_slice_parser_provider_hash().unwrap()),
            InputFingerprint::new(
                InputKey::ResolverVersion,
                content_hash(RESOLVER_BINARY_SEED),
            ),
            InputFingerprint::new(
                InputKey::ConfigurationRevision,
                content_hash(b"unchanged-configuration"),
            ),
            InputFingerprint::new(InputKey::SearchRevision, content_hash(SEARCH_REVISION_SEED)),
        ],
        limits,
        &cancellation,
    )
    .unwrap();
    let keys = incremental_input_keys(&current, &current, &cancellation).unwrap();
    let passes = first_slice_passes().unwrap();
    let (nodes, edges) = first_slice_dependency_parts(
        repository,
        &BTreeSet::from([file]),
        &keys,
        &passes,
        &cancellation,
    )
    .unwrap();
    let graph_limits = GraphLimits::new(5, nodes.len(), edges.len()).unwrap();
    let registry =
        DependencyRegistry::new(passes.declarations().unwrap(), graph_limits, &cancellation)
            .unwrap();
    verify_first_slice_observations(&registry, &passes).unwrap();
    let graph = DependencyGraph::new(nodes, edges, &registry, graph_limits, &cancellation).unwrap();
    let artifact_id = parser_artifact_id(file);
    let summary = |inputs: InputSnapshot| {
        let artifact = ArtifactSummary::new(
            artifact_id,
            [FactNode::new(file_analysis_unit(file), FactDomain::Syntax)],
            inputs
                .iter()
                .filter(|input| parser_artifact_depends_on(input.key(), file)),
            limits,
            &cancellation,
        )
        .unwrap();
        GenerationSummary::new(inputs, [artifact], limits, &cancellation).unwrap()
    };
    let unchanged = summary(current.clone());
    let reuse = plan_invalidation(&unchanged, &current, &graph, limits, &cancellation).unwrap();
    assert!(reuse.changes().changes().is_empty());
    assert_eq!(reuse.invalidated_nodes().count(), 0);
    assert_eq!(reuse.artifact_decisions().len(), 1);
    assert_eq!(
        reuse.artifact_decisions()[0].kind(),
        ArtifactDecisionKind::Reuse
    );

    let previous = InputSnapshot::new(
        current.iter().map(|input| {
            if input.key() == adapter_key {
                InputFingerprint::new(adapter_key, written_field_296_parser_hash())
            } else {
                input
            }
        }),
        limits,
        &cancellation,
    )
    .unwrap();
    for input in current.iter().filter(|input| input.key() != adapter_key) {
        assert_eq!(previous.value(input.key()), Some(input.value()));
    }
    assert_eq!(previous.value(grammar_key), Some(grammar));
    let parent = summary(previous);
    let plan = plan_invalidation(&parent, &current, &graph, limits, &cancellation).unwrap();
    assert_eq!(
        plan.changes()
            .changes()
            .iter()
            .map(|change| change.key())
            .collect::<Vec<_>>(),
        vec![adapter_key]
    );
    assert!(plan.fallback().is_none());
    assert_eq!(plan.artifact_decisions().len(), 1);
    assert_eq!(plan.artifact_decisions()[0].artifact(), artifact_id);
    assert_eq!(
        plan.artifact_decisions()[0].kind(),
        ArtifactDecisionKind::Rebuild
    );
    assert!(
        plan.invalidated_nodes()
            .any(|node| node == FactNode::new(file_analysis_unit(file), FactDomain::Syntax))
    );
}

#[test]
fn written_fields_analyzer_change_reaches_both_deep_project_contexts() {
    let unavailable = FirstSliceService::new(2).unwrap();
    let identity = content_hash(b"unchanged-project-analyzer");
    let configured = FirstSliceService::new_with_storage(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        Some(Arc::new(FailingProjectAnalyzer {
            identity,
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::new(AtomicUsize::new(0)),
        })),
    )
    .unwrap();
    let provider = |parser: ContentHash| {
        hash_static_components(&[
            PROVIDER_SET_SEED,
            parser.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            RESOLVER_BINARY_SEED,
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap()
    };
    assert_eq!(
        first_slice_provider_set_hash().unwrap(),
        provider(first_slice_parser_provider_hash().unwrap())
    );
    let previous_provider = provider(written_field_296_parser_hash());
    for (service, project) in [
        (&unavailable, b"unavailable".as_slice()),
        (&configured, identity.as_bytes().as_slice()),
    ] {
        let policy = crate::aggregate_source_recovery::policy_hash(
            service.analysis_limits.ir(),
            service.generation_budget,
        )
        .unwrap();
        let previous_structural =
            hash_static_components(&[previous_provider.as_bytes(), policy.as_bytes()]).unwrap();
        let current_structural = service
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap();
        assert_ne!(current_structural, previous_structural);
        let current_deep = service
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap();
        assert_eq!(
            current_deep,
            hash_static_components(&[
                PROJECT_PROVIDER_SET_SEED,
                current_structural.as_bytes(),
                project,
            ])
            .unwrap()
        );
        let previous_deep = hash_static_components(&[
            PROJECT_PROVIDER_SET_SEED,
            previous_structural.as_bytes(),
            project,
        ])
        .unwrap();
        assert_ne!(current_deep, previous_deep);
        assert_ne!(current_deep, current_structural);
    }
    assert_eq!(
        unavailable
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap(),
        configured
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap()
    );
    assert_ne!(
        unavailable
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap(),
        configured
            .provider_set_hash(FirstSliceIndexMode::Deep)
            .unwrap()
    );
}

#[test]
fn rust_bom_proof_context_invalidates_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/324",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn go_function_value_references_invalidate_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/325",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn complete_native_query_captures_invalidate_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/326",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn c_declaration_proofs_invalidate_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/327",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn written_native_semantics_invalidate_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/328",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn typescript_const_proofs_invalidate_preceding_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/330",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/141")
    );
}

#[test]
fn bash_written_wrapper_proofs_invalidate_preceding_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/331",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/142")
    );
}

#[test]
fn eiffel_formal_receiver_reads_invalidate_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/332",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn cuda_native_proofs_invalidate_preceding_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/333",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/143")
    );
}

#[test]
fn c_local_address_proofs_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/334",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/144")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/144",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn python_module_main_references_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/335",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/145")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/145",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn php_ordinary_source_proofs_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/336",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        PROJECT_PROVIDER_SET_SEED,
        b"rootlight.first-slice.project-provider/32"
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/146")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/146",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn javascript_written_bindings_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/337",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        PROJECT_PROVIDER_SET_SEED,
        b"rootlight.first-slice.project-provider/32"
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/147")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/147",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn javascript_default_class_export_proofs_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/338",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        PROJECT_PROVIDER_SET_SEED,
        b"rootlight.first-slice.project-provider/32"
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/148")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/148",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn rust_struct_member_ownership_invalidates_immediate_native_and_deep_contexts() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/339",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        PROJECT_PROVIDER_SET_SEED,
        b"rootlight.first-slice.project-provider/32"
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/149")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/149",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
    let unavailable = FirstSliceService::new(2).unwrap();
    let project_identity = content_hash(b"unchanged-rust-project-provider");
    let configured = FirstSliceService::new_with_storage(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        Some(Arc::new(FailingProjectAnalyzer {
            identity: project_identity,
            error: FirstSliceProjectAnalysisError::Analysis,
            calls: Arc::new(AtomicUsize::new(0)),
        })),
    )
    .unwrap();
    let contexts: [(&FirstSliceService, &[u8]); 2] = [
        (&unavailable, b"unavailable"),
        (&configured, project_identity.as_bytes()),
    ];
    for (service, project) in contexts {
        let structural = service
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap();
        let previous = hash_static_components(&[
            b"rootlight.first-slice.project-provider/29",
            structural.as_bytes(),
            project,
        ])
        .unwrap();
        assert_ne!(
            service
                .provider_set_hash(FirstSliceIndexMode::Deep)
                .unwrap(),
            previous
        );
    }
}

#[test]
fn ruby_written_bindings_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/340",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        PROJECT_PROVIDER_SET_SEED,
        b"rootlight.first-slice.project-provider/32"
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/150")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/150",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn bash_module_references_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/341",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        PROJECT_PROVIDER_SET_SEED,
        b"rootlight.first-slice.project-provider/32"
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/151")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/151",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn html_closing_tag_references_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/342",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        PROJECT_PROVIDER_SET_SEED,
        b"rootlight.first-slice.project-provider/32"
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/152")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/152",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn assembly_written_owners_invalidate_immediate_parser_and_resolver_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/343",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        PROJECT_PROVIDER_SET_SEED,
        b"rootlight.first-slice.project-provider/32"
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(parser, previous_parser);
    assert_ne!(
        content_hash(RESOLVER_BINARY_SEED),
        content_hash(b"rootlight.first-slice.resolve/153")
    );
    for structural in [previous_parser, parser] {
        let previous = hash_static_components(&[
            PROVIDER_SET_SEED,
            structural.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            b"rootlight.first-slice.resolve/153",
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap();
        assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
    }
}

#[test]
fn native_refusal_recognition_invalidates_immediate_host_admission_identity() {
    let parser = first_slice_parser_provider_hash().unwrap();
    let provider = |policy: &[u8]| {
        hash_static_components(&[
            policy,
            parser.as_bytes(),
            DISCOVERY_MANIFEST_VERSION.as_bytes(),
            RESOLVER_PROVIDER_VERSION.as_bytes(),
            RESOLVER_BINARY_SEED,
            SEARCH_REVISION_SEED,
            INCREMENTAL_SCHEMA_VERSION.as_bytes(),
            DERIVED_PLAN_REVISION_SEED,
            LANGUAGE_DISPOSITION_PROVIDER_SEED,
            SOURCE_FILE_FALLBACK_PROVIDER_SEED,
            container_sources::REVISION,
        ])
        .unwrap()
    };
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_eq!(
        first_slice_provider_set_hash().unwrap(),
        provider(PROVIDER_SET_SEED)
    );
    assert_ne!(
        first_slice_provider_set_hash().unwrap(),
        provider(b"rootlight.first-slice.providers/6")
    );
    assert_ne!(
        first_slice_provider_set_hash().unwrap(),
        provider(b"rootlight.first-slice.providers/10")
    );
}

#[test]
fn python_formal_rebinding_invalidates_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let preceding_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/347",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(
        first_slice_parser_provider_hash().unwrap(),
        preceding_parser
    );
    let preceding_provider = hash_static_components(&[
        PROVIDER_SET_SEED,
        preceding_parser.as_bytes(),
        DISCOVERY_MANIFEST_VERSION.as_bytes(),
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        container_sources::REVISION,
    ])
    .unwrap();
    assert_ne!(first_slice_provider_set_hash().unwrap(), preceding_provider);
}

#[test]
fn python_module_iterables_invalidate_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let preceding_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/348",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(PROVIDER_SET_SEED, b"rootlight.first-slice.providers/11");
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/155");
    assert_ne!(
        first_slice_parser_provider_hash().unwrap(),
        preceding_parser
    );
    let preceding_provider = hash_static_components(&[
        PROVIDER_SET_SEED,
        preceding_parser.as_bytes(),
        DISCOVERY_MANIFEST_VERSION.as_bytes(),
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        container_sources::REVISION,
    ])
    .unwrap();
    assert_ne!(first_slice_provider_set_hash().unwrap(), preceding_provider);
}

#[test]
fn nushell_file_module_ownership_invalidates_preceding_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/352",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn erb_combined_ruby_invalidates_same_grammar_host_only_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let host_only = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/353",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_ne!(first_slice_parser_provider_hash().unwrap(), host_only);
}

#[test]
fn erb_block_reads_invalidate_same_grammar_syntax_only_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/354",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn markdown_host_completeness_invalidates_same_grammar_aggregate_only_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/355",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}

#[test]
fn markdown_handlebars_attribution_invalidates_same_grammar_parser_identity() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let previous = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/356",
        grammar.as_bytes(),
    ])
    .unwrap();
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/362"
    );
    assert_ne!(first_slice_parser_provider_hash().unwrap(), previous);
}
