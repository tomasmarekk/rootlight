//! Perl callable edges must reserve the same quota used by materialization.
//! Native-shaped facts exercise both direct and embedded lowering independently
//! of parser capture discovery.

use super::*;

#[test]
fn perl_callable_edges_reserve_quotas_before_materialization() {
    assert_callable_edge_quotas(
        "sub entry { 1 } entry(); my $ref = \\&entry;",
        "perl.static_function_name.reference",
        "entry",
    );
}

#[test]
fn perl_bare_callable_edges_reserve_quotas_before_materialization() {
    assert_callable_edge_quotas(
        "sub entry { 1 } entry; my $ref = \\&entry;",
        "perl.bare_function_name.reference",
        "entry",
    );
}

#[test]
fn perl_qualified_callable_edges_reserve_quotas_before_materialization() {
    assert_callable_edge_quotas(
        "sub main::Cove::entry { 1 } main::Cove::entry(); my $ref = \\&main::Cove::entry;",
        "perl.static_function_name.reference",
        "main::Cove::entry",
    );
}

#[test]
fn perl_direct_coderef_edges_reserve_quotas_before_materialization() {
    assert_callable_edge_quotas_at(
        "sub entry { 1 } (\\&entry)->(); my $ref = \\&entry;",
        "perl.direct_coderef_function_name.reference",
        "entry",
        ("&entry", 0),
        1,
    );
}

fn assert_callable_edge_quotas(perl: &str, call_kind: &str, name: &str) {
    assert_callable_edge_quotas_at(perl, call_kind, name, (name, 1), 0);
}

fn assert_callable_edge_quotas_at(
    perl: &str,
    call_kind: &str,
    name: &str,
    call: (&str, usize),
    reference_skip: usize,
) {
    let (_temporary, snapshot, source) =
        source_fixture_for(perl, "src/module.pm", b"perl-callable-quotas");
    let declaration = format!("sub {name} {{ 1 }}");
    let code_reference = format!("&{name}");
    let facts: Vec<_> = [
        (1, None, SyntaxFactKind::Root, perl, 0, 0, "perl.file.root"),
        (
            2,
            Some(1),
            SyntaxFactKind::Module,
            perl,
            0,
            1,
            "perl.file.module",
        ),
        (
            3,
            Some(2),
            SyntaxFactKind::Scope,
            perl,
            0,
            2,
            "perl.file.scope",
        ),
        (
            4,
            Some(3),
            SyntaxFactKind::Scope,
            declaration.as_str(),
            0,
            3,
            "perl.function.scope",
        ),
        (
            5,
            Some(4),
            SyntaxFactKind::Declaration,
            declaration.as_str(),
            0,
            4,
            "perl.function.declaration",
        ),
        (
            6,
            Some(5),
            SyntaxFactKind::Occurrence,
            name,
            0,
            5,
            "perl.identifier.definition",
        ),
        (
            7,
            Some(3),
            SyntaxFactKind::Occurrence,
            call.0,
            call.1,
            3,
            call_kind,
        ),
        (
            8,
            Some(3),
            SyntaxFactKind::Occurrence,
            code_reference.as_str(),
            reference_skip,
            3,
            "perl.code_function_name.reference",
        ),
    ]
    .into_iter()
    .map(|(id, parent, kind, text, nth, depth, syntax)| {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span_in(perl, &source, text, nth),
            depth,
            label(syntax),
        )
    })
    .collect();
    for host in ["perl", "markdown"] {
        let language = LanguageId::new(host).unwrap();
        let output = analyze_custom(
            &snapshot,
            &source,
            language.clone(),
            &limits(IrLimits::default()),
            facts.clone(),
        )
        .unwrap();
        for predicate in [RelationPredicate::Calls, RelationPredicate::RefersTo] {
            assert_eq!(
                output
                    .document()
                    .relations
                    .iter()
                    .filter(|edge| edge.predicate == predicate)
                    .count(),
                1
            );
        }
        let total = output.document().relations.len();
        let mut ir = IrLimits::default();
        ir.max_relations = total - 1;
        let error = analyze_custom(&snapshot, &source, language, &limits(ir), facts.clone())
            .expect_err("callable edges cannot bypass the relation quota");
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: rootlight_adapter_sdk::ResourceKind::Records, observed, limit }) if observed >= total && limit == total - 1),
            "{error:?}"
        );
    }
}
