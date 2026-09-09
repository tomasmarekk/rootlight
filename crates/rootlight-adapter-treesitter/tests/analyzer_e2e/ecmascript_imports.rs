//! Source-backed local import declarations for both ECMAScript identities.
//! Imported export names and re-exports must not invent local bindings; native
//! type-only tokens remain distinct from ordinary identifiers named type.

use super::*;

#[test]
fn ecmascript_imports_replay_with_exact_source_and_required_budget() {
    let source = "import {名 as value, type as ordinary} from './types';\r\nimport * as Space from './ns';\r\n";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
        let required = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        let bounded = limits_with_syntax_records(required);
        let initial = request(&fixture.snapshot, &fixture.source, case, &bounded);
        let (first, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            artifact.required_syntax_fact_count(&deadline()).unwrap(),
            required
        );
        let import_ids = |document: &rootlight_ir::NormalizedIrDocument| {
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Import)
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(import_ids(first.document()).len(), 3);
        let next = fixture.next_generation();
        let next_request = request(&next.snapshot, &next.source, case, &bounded);
        let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
        let replay = analyzer
            .analyze_from_artifact(
                &next_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(fresh.document(), replay.document());
        assert_eq!(fresh.report(), replay.report());
        assert_eq!(import_ids(first.document()), import_ids(replay.document()));
        for occurrence in &replay.document().occurrences {
            assert_eq!(occurrence.source.generation(), next.source.generation());
            assert_eq!(
                occurrence.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
        let insufficient = limits_with_syntax_records(required.checked_sub(1).unwrap());
        let error = analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case, &insufficient),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap_err();
        assert!(
            matches!(error, AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit { resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1),
            "{error:?}"
        );
        let edited = format!(
            "const unrelated = 1;\r\n{}",
            source.replace("./types", "./other")
        );
        let variant = fixture.rewrite(edited.as_bytes());
        let changed = analyze(
            &analyzer,
            &request(&variant.snapshot, &variant.source, case, &budget),
            &ExtensionSupport::default(),
        );
        assert_eq!(import_ids(first.document()), import_ids(changed.document()));
    }
}

#[test]
fn typescript_imports_distinguish_type_tokens_from_identifiers() {
    let source = "import type DefaultType from './types';\r\nimport type * as NamespaceType from './types';\r\nimport {type Named as NamedType, type PlainType, value as runtimeValue, type as typeValue} from './mixed';\r\nimport type from './value';\r\nimport Required = require('./required');\r\nimport Alias = Existing.Member;\r\n";
    let case = *CASES.iter().find(|case| case.name == "typescript").unwrap();
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let analysis_request = request(&fixture.snapshot, &fixture.source, case, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &analysis_request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let labels: BTreeMap<_, _> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Declaration)
        .map(|fact| {
            let span = fact.span();
            (
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                fact.syntax_kind().as_str(),
            )
        })
        .collect();
    let typed = "typescript.type_import_binding.declaration";
    let value = "typescript.import_binding.declaration";
    assert_eq!(
        labels,
        BTreeMap::from([
            ("DefaultType", typed),
            ("NamespaceType", typed),
            ("NamedType", typed),
            ("PlainType", typed),
            ("runtimeValue", value),
            ("typeValue", value),
            ("type", value),
            ("Required", value),
            ("Alias", value),
        ])
    );
    let result = analyze(&analyzer, &analysis_request, &ExtensionSupport::default());
    let names: BTreeSet<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Import)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names, labels.keys().copied().collect());
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .all(|gap| gap.domain != FactDomain::Entities),
        "{:#?}",
        result.document().skipped_regions
    );
}

#[test]
fn ecmascript_imports_define_only_the_local_written_names() {
    let source = "import Default from './default.js'; import * as Namespace from './namespace.js'; import {plain, original as renamed, 'external-name' as quoted} from './named.js'; import './side-effect.js'; export {foreign as reexported} from './remote.js'; const unrelated = 1;";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let analysis_request = request(&fixture.snapshot, &fixture.source, case, &budget);
        let parsed = rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &analysis_request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let foreign: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind()
                    .as_str()
                    .ends_with(".import_name.reference")
            })
            .collect();
        assert_eq!(foreign.len(), 1);
        let span = foreign[0].span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "original"
        );
        let result = analyze(&analyzer, &analysis_request, &ExtensionSupport::default());
        let document = result.document();
        let imports: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Import)
            .collect();
        assert_eq!(
            imports
                .iter()
                .map(|entity| entity.canonical_name.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["Default", "Namespace", "plain", "renamed", "quoted"]),
            "{}: {:#?}",
            case.name,
            document.skipped_regions
        );
        assert_eq!(imports.len(), 5);
        for import in imports {
            let definition = document
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: import.id }
                })
                .unwrap();
            let span = definition.source.span();
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(text, import.canonical_name);
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash(text.as_bytes())
            );
        }
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations
                    && gap.detail == "unresolved-import-target")
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .all(|gap| gap.domain != FactDomain::Entities),
            "{:#?}",
            document.skipped_regions
        );
    }
}
