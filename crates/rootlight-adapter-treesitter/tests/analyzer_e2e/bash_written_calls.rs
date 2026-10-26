//! Written shell wrapper dependencies retain exact physical evidence.
//! Complete upstream bytes and renamed controls exercise the public analyzer;
//! runtime evaluation, external lookup and dynamic command binding remain unqualified.

use super::*;
use rootlight_ir::{FactRef, derive_occurrence_record_id, derive_relation_record_id};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/bash_written_calls/nvm.sh");
const CALL: &str = "bash.direct_wrapper_call.call";

fn wrapper_limits() -> AnalysisLimits {
    // The complete upstream file exceeds the shared tiny-fixture census bound.
    // Keep this runner finite without changing production or exact-fit controls.
    let stream = StreamLimits::new(
        4096,
        262144,
        64 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap(),
    )
    .unwrap();
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        1_048_576,
        4096,
        32,
        64 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
}

fn wrapper_provider() -> TreeSitterProvider {
    TreeSitterProvider::new(
        RuntimeConfig::new(
            MAX_SOURCE_BYTES,
            1_048_576,
            4096,
            32,
            64,
            1,
            64 * 1024 * 1024,
            ParserSettings::new(4096).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn run(raw: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let parser = Arc::new(wrapper_provider());
    let fixture = Fixture::new(BASH_CASE, raw);
    let budget = wrapper_limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(BASH_CASE),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let output = analyze(
        &analyzer(&parser, BASH_CASE),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    output
}

fn assert_call(
    output: &AnalysisOutput,
    raw: &[u8],
    owner_name: &str,
    target_name: &str,
    start: usize,
    end: usize,
) {
    let document = output.document();
    let entity = |name: &str| {
        let found: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "bash"
                    && entity.kind == EntityKind::Function
                    && entity.canonical_name == name
            })
            .collect();
        assert_eq!(found.len(), 1, "unique authored function {name}");
        found[0]
    };
    let (owner, target) = (entity(owner_name), entity(target_name));
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == CALL
                && site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "exact certified wrapper invocation {start}..{end}"
    );
    let site = sites[0];
    assert_eq!(site.enclosing, Some(owner.id));
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.provenance, owner.provenance);
    assert_eq!(site.id, derive_occurrence_record_id(site).unwrap());
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Calls
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(target.id)
                && relation.evidence.source.as_ref() == Some(&site.source)
        })
        .collect();
    assert_eq!(relations.len(), 1, "one source-owned Calls edge");
    let relation = relations[0];
    let mut proof = vec![
        FactRef::Entity(owner.id),
        FactRef::Fact(site.id),
        FactRef::Entity(target.id),
    ];
    proof.sort_unstable();
    assert_eq!(relation.evidence.derivation, proof);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn complete_original_nvm_wrappers_keep_written_direct_calls_and_original_eof() {
    assert_eq!(ORIGINAL.len(), 168666);
    assert_eq!(&ORIGINAL[732..749], b">&2 nvm_echo \"$@\"");
    assert_eq!(&ORIGINAL[779..808], b">&2 nvm_echo_with_colors \"$@\"");
    let output = run(ORIGINAL, EncodingId::utf8());
    assert_call(&output, ORIGINAL, "nvm_err", "nvm_echo", 732, 749);
    assert_call(
        &output,
        ORIGINAL,
        "nvm_err_with_colors",
        "nvm_echo_with_colors",
        779,
        808,
    );
    assert!(
        output
            .document()
            .files
            .iter()
            .any(|file| file.byte_length == 168666)
    );
    assert!(output.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Relations
            && gap.detail == "bash-module-parameter-binding-unavailable"
            && gap.source.content_hash() == content_hash(ORIGINAL)
            && output.document().occurrences.iter().any(|site| {
                site.source == gap.source
                    && site.role == OccurrenceRole::Read
                    && site.syntax_kind == "bash.unproven_read.reference"
                    && matches!(site.target, OccurrenceTarget::Unresolved { .. })
            })
    }));
}

#[test]
fn renamed_wrapper_pair_preserves_call_span_for_utf8_bom_crlf_and_utf16() {
    let logical = "{\r\nrenamed_target() \t { :; }\r\nrenamed_wrapper()  { >&2 renamed_target \"$@\"; }\r\n}\r\n";
    let command = ">&2 renamed_target \"$@\"";
    let start = logical.find(command).unwrap();
    for (raw, encoding, offset, width) in [
        (
            logical.as_bytes().to_vec(),
            EncodingId::utf8(),
            0usize,
            1usize,
        ),
        (
            [b"\xef\xbb\xbf".as_slice(), logical.as_bytes()].concat(),
            EncodingId::utf8(),
            3,
            1,
        ),
        (
            [
                b"\xff\xfe".as_slice(),
                logical
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
                    .as_slice(),
            ]
            .concat(),
            EncodingId::new("utf-16le").unwrap(),
            2,
            2,
        ),
    ] {
        let output = run(&raw, encoding);
        assert_call(
            &output,
            &raw,
            "renamed_wrapper",
            "renamed_target",
            offset + width * start,
            offset + width * (start + command.len()),
        );
    }
}

#[test]
fn unrelated_unexecuted_unset_body_does_not_erase_authored_dependency() {
    let source =
        b"target(){ :; }\nwrapper(){ >&2 target \"$@\"; }\ncleanup(){ unset -f target; }\n";
    let command = b">&2 target \"$@\"";
    let start = source
        .windows(command.len())
        .position(|raw| raw == command)
        .unwrap();
    let output = run(source, EncodingId::utf8());
    assert_call(
        &output,
        source,
        "wrapper",
        "target",
        start,
        start + command.len(),
    );
}

#[test]
fn dynamic_commands_and_control_flow_keep_exact_wrapper_calls_unavailable() {
    for body in [
        "command target",
        "builtin target",
        "eval target",
        "source target",
        "\"target\"",
        "$target",
        "X=x target",
        "target \"$(other)\"",
        "target `other`",
        "target ${value:=changed}",
        "target <(other)",
        "target >file",
        "target >&\"$fd\"",
        "target | other",
        "target && other",
        "(target)",
        "if flag; then target; fi",
        "target; other",
        "target &",
    ] {
        let terminator = if body.ends_with('&') { "" } else { ";" };
        let source = format!("target(){{ :; }}\nwrapper(){{ {body}{terminator} }}\n");
        let output = run(source.as_bytes(), EncodingId::utf8());
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == CALL
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls),
            "{source}"
        );
    }
}

#[test]
fn wrapper_required_artifacts_match_independent_preflight_and_survive_exact_fact_budget() {
    let source = b"target(){ :; }\nwrapper(){ >&2 target \"$@\"; }\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, BASH_CASE);
    let fixture = Fixture::new(BASH_CASE, source);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, BASH_CASE, &budget);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap()
    );
    assert!(
        fresh
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == CALL
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let bounded = limits_with_syntax_records(required);
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, BASH_CASE, &bounded);
    let (captured, next_artifact) = analyzer
        .analyze_and_capture(
            &next_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        next_artifact
            .required_syntax_fact_count(&deadline())
            .unwrap(),
        required
    );
    assert!(
        captured
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == CALL
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let reused = analyzer
        .analyze_from_artifact(
            &next_request,
            &next_artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(captured.document(), reused.document());
    let insufficient = limits_with_syntax_records(required - 1);
    let rejected = analyzer.prepare_artifact(
        &request(&next.snapshot, &next.source, BASH_CASE, &insufficient),
        &deadline(),
    );
    assert!(
        matches!(rejected, Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit })) if observed == required && limit == required - 1)
    );
}

#[test]
fn certified_native_identity_pattern_keeps_unrelated_default_command_occurrences() {
    let source = b"target(){ :; }\nwrapper(){ >&2 target \"$@\"; }\nexternal_command argument\n";
    let output = run(source, EncodingId::utf8());
    let external = source
        .windows(b"external_command".len())
        .position(|token| token == b"external_command")
        .unwrap();
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "bash.call.call"
                && site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == u64::try_from(external).unwrap())
    );
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == CALL
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn valid_background_function_and_outer_group_definitions_keep_calls_unavailable() {
    for source in [
        "target(){ :; }\nwrapper(){ target; } &\n",
        "{ target(){ :; }; wrapper(){ target; }; } &\n",
        "target(){ :; } &\nwrapper(){ target; }\n",
        "target(){ :; }\nwrapper(){ target; }\nexternal &\n",
        "target(){ :; }\nwrapper(){ target & }\n",
    ] {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .unwrap();
        assert!(
            !parser.parse(source, None).unwrap().root_node().has_error(),
            "valid async syntax premise: {source}"
        );
        let output = run(source.as_bytes(), EncodingId::utf8());
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == CALL
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{source}"
        );
    }
    let source = b"target(){ :; }\nwrapper(){ target; }\nother(){ external & }\n";
    let command = b"target";
    let start = source
        .windows(command.len())
        .rposition(|word| word == command)
        .unwrap();
    assert_call(
        &run(source, EncodingId::utf8()),
        source,
        "wrapper",
        "target",
        start,
        start + command.len(),
    );
}

// Companion proof work must preserve the complete wrapper source at its existing limits.
// Function-owned variable uses are excluded before a module companion census.

#[test]
fn complete_nvm_wrapper_proofs_survive_companion_census_at_existing_budgets() {
    assert_eq!(ORIGINAL.len(), 168666);
    let output = run(ORIGINAL, EncodingId::utf8());
    assert_call(&output, ORIGINAL, "nvm_err", "nvm_echo", 732, 749);
    assert_call(
        &output,
        ORIGINAL,
        "nvm_err_with_colors",
        "nvm_echo_with_colors",
        779,
        808,
    );
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "bash.module_reference.reference"
                && site.enclosing.is_some_and(|id| output
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.id == id && entity.kind == EntityKind::Function))),
        "module companion admission cannot cross a written function owner"
    );
    assert!(
        !output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "query-identity-bash-reads-limit"),
        "new work cannot erase previously qualified complete wrapper proofs"
    );
}
