//! Julia integration checks written ownership and precise source provenance.
//! Runtime dispatch and evaluation remain explicit gaps even for matching names.

use super::*;

const JULIA: LanguageCase = LanguageCase {
    name: "julia",
    path: "src/written.jl",
    frontend: "tree-sitter-julia-0.25.0",
    source: "# marker λ😀\nmodule Outer\nabstract type Parent end\nprimitive type Byte <: Parent 8 end\nmutable struct Box{T} <: Parent\n value::T\n Box(x::T) where T = new{T}(x)\nend\nfunction Base.take!(b::Box{T}) where T\n local_value = b.value\n local_value\nend\nι(x) = x\n⊗(x, y) = x * y\nmacro identity(ex)\n esc(ex)\nend\nbaremodule Inner end\nend\n",
    generated: false,
    body_before: "local_value\nend",
    body_after: "nothing\nend",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(JULIA, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, JULIA),
        &request(&fixture.snapshot, &fixture.source, JULIA, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &document.occurrences {
        let span = occurrence.source.span();
        let bytes = source
            .as_bytes()
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(occurrence.syntactic_text_hash, content_hash(bytes));
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_eq!(occurrence.source.generation(), document.generation);
        if occurrence.role != OccurrenceRole::Definition {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    result
}

#[test]
fn julia_explicit_globals_keep_module_ownership_and_exact_written_sites() {
    let source = "module Globals\nglobal active::Any\nglobal alpha, beta\nfunction write()\n global active = 1\n global chosen::Int\n global left, right = pair()\n local kept = 3\nend\nmodule Inner\n function set()\n  global nested\n end\nend\nquote\n global quoted_name\nend\nend\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (anchor, name, owner_name) in [
        ("active::Any", "active", "Globals"),
        ("alpha, beta", "alpha", "Globals"),
        ("beta\n", "beta", "Globals"),
        ("active = 1", "active", "Globals"),
        ("chosen::Int", "chosen", "Globals"),
        ("left, right", "left", "Globals"),
        ("right = pair", "right", "Globals"),
        ("kept = 3", "kept", "write"),
        ("nested\n", "nested", "Inner"),
    ] {
        let start = u64::try_from(source.find(anchor).unwrap()).unwrap();
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start
                    && occurrence.source.span().end_byte()
                        == start + u64::try_from(name.len()).unwrap()
            })
            .collect();
        assert_eq!(
            definitions.len(),
            1,
            "missing exact global/local declaration {anchor}"
        );
        let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
            panic!("unresolved written global")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.kind, EntityKind::Variable);
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner_name)
            .unwrap();
        assert!(
            document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(entity.id)
            }),
            "wrong source owner for {anchor}"
        );
    }
    for forbidden in ["Any", "Int", "quoted_name"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == forbidden)
        );
    }
    assert_eq!(
        document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == FactDomain::Relations
                    && gap.detail == "julia-global-binding-target-unavailable"
            })
            .count(),
        6
    );
}

#[test]
fn julia_quoted_identifiers_keep_decoded_names_and_full_written_tokens() {
    let source = r#"module Names
struct Fields
 var"field name"
 var"typed field"::Int
end
const var"binding space" = 1
function Base.var"@named"(var"arg-name"::Int)
 var"arg-name"
end
var"λ😀"(x) = x
var"a\"b" = 2
var"trailing\\" = 3
var"raw\x" = 4
var"$literal" = 5
Base.var"@named"(var"binding space")
raw"not-name" = 6
var"bad"suffix = 7
var"""triple""" = 8
quote
 var"quoted-name" = 9
end
end
"#;
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (written, canonical, kind) in [
        (r#"var"field name""#, "field name", EntityKind::Field),
        (r#"var"typed field""#, "typed field", EntityKind::Field),
        (
            r#"var"binding space""#,
            "binding space",
            EntityKind::Variable,
        ),
        (r#"var"@named""#, "@named", EntityKind::Function),
        (r#"var"arg-name""#, "arg-name", EntityKind::Parameter),
        (r#"var"λ😀""#, "λ😀", EntityKind::Function),
        (r#"var"a\"b""#, "a\"b", EntityKind::Variable),
        (r#"var"trailing\\""#, "trailing\\", EntityKind::Variable),
        (r#"var"raw\x""#, "raw\\x", EntityKind::Variable),
        (r#"var"$literal""#, "$literal", EntityKind::Variable),
    ] {
        let start = u64::try_from(source.find(written).unwrap()).unwrap();
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start
                    && occurrence.source.span().end_byte()
                        == start + u64::try_from(written.len()).unwrap()
            })
            .collect();
        assert_eq!(
            definitions.len(),
            1,
            "missing exact quoted declaration {written}"
        );
        let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
            panic!("unresolved written quoted declaration")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.canonical_name, canonical);
        assert_eq!(entity.kind, kind);
        assert!(!document.occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::StringEvidence
                && occurrence.source.span().start_byte() == start
        }));
    }
    for forbidden in [
        "var",
        "raw",
        "not-name",
        "bad",
        "suffix",
        "triple",
        "quoted-name",
        "Int",
    ] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == forbidden),
            "invented {forbidden}"
        );
    }
    let function = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "@named")
        .unwrap();
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "arg-name")
        .unwrap();
    assert!(document.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Contains
            && relation.subject == RelationEndpoint::Entity(function.id)
            && relation.object == RelationEndpoint::Entity(parameter.id)
    }));
    let call = document
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .unwrap();
    let span = call.source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        r#"var"@named""#
    );
}

#[test]
fn julia_modules_methods_types_fields_parameters_and_operators_have_exact_names() {
    let result = output(JULIA.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("Outer", EntityKind::Module),
        ("Parent", EntityKind::Class),
        ("Byte", EntityKind::Struct),
        ("Box", EntityKind::Struct),
        ("value", EntityKind::Field),
        ("Box", EntityKind::Function),
        ("x", EntityKind::Parameter),
        ("take!", EntityKind::Function),
        ("b", EntityKind::Parameter),
        ("local_value", EntityKind::Variable),
        ("ι", EntityKind::Function),
        ("⊗", EntityKind::Function),
        ("identity", EntityKind::Function),
        ("ex", EntityKind::Parameter),
        ("Inner", EntityKind::Module),
    ] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.kind == kind),
            "missing {name} {kind:?}: {:?}; gaps {:?}",
            document
                .entities
                .iter()
                .map(|entity| (&entity.canonical_name, entity.kind))
                .collect::<Vec<_>>(),
            document.skipped_regions
        );
    }
    for occurrence in document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
    {
        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
            panic!("unresolved declaration")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        let span = occurrence.source.span();
        assert_eq!(
            &JULIA.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            entity.canonical_name
        );
    }
}

#[test]
fn julia_literal_decoys_quotes_macro_operands_and_runtime_calls_remain_distinct() {
    let source = "# function commented() end\ndoc = \"\"\"function literal() end\"\"\"\nquote\n function quoted() end\nend\n@inline written(x) = x\nfunction run(value)\n written(value)\nend\nusing Base: show\n";
    let result = output(source);
    for forbidden in ["commented", "literal", "quoted"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == forbidden)
        );
    }
    assert!(result.document().entities.iter().any(|entity| entity.canonical_name == "written" && entity.kind == EntityKind::Function));
    let calls: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(result.document().skipped_regions.iter().any(|gap| gap.detail == "julia-macro-expansion-and-generated-declarations-unavailable"));
}

#[test]
fn julia_body_edits_and_unrelated_declarations_preserve_written_symbol_ids() {
    let before = output("function written(x::Int)\n x + 1\nend\nwritten(x::String) = x\n");
    let after =
        output("unrelated = 42\nfunction written(x::Int)\n x + 2\nend\nwritten(x::String) = x\n");
    let identities = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "written")
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(identities(&before).len(), 2);
    assert_eq!(identities(&before), identities(&after));
}

#[test]
fn julia_recovered_bodies_keep_proven_headers_and_missing_ends_remain_gaps() {
    // The compact let form occurs in Julia's CFunction declaration.
    let source = "mutable struct CFunction <: Ref{Cvoid}\n ptr::Ptr{Cvoid}\n let constructor = false end\nend\nfunction main(args = ARGS)::Int\n value = ?\nend\n";
    let result = output(source);
    assert!(
        result
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
    for name in ["CFunction", "main"] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "missing recovered owner {name}"
        );
    }
    let incomplete = output("function missing(x)\n x\n");
    assert!(
        !incomplete
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "missing")
    );
    assert!(!incomplete.document().diagnostics.is_empty());
}

#[test]
fn julia_callable_receiver_bindings_and_qualified_header_evidence_remain_written() {
    let source = "function (f::Fix{N})(args::Vararg{Any,M}; kws...) where {N,M}\n f\nend\n@inline function Base.getindex(F::Fill{<:Any,N}, i::Vararg{Int,N}) where {N}\n F.value\nend\nconst ∈ = in\nconst % = rem\n";
    let result = output(source);
    for (name, kind) in [
        ("f", EntityKind::Parameter),
        ("args", EntityKind::Parameter),
        ("kws", EntityKind::Parameter),
        ("getindex", EntityKind::Function),
        ("∈", EntityKind::Variable),
        ("%", EntityKind::Variable),
    ] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.kind == kind),
            "missing {name} {kind:?}"
        );
    }
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "f" && entity.kind == EntityKind::Function)
    );
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "<anonymous>")
    );
}

#[test]
fn julia_property_mutations_do_not_invent_named_variable_definitions() {
    let source = "function update!(object, values, index)\n object.value = 1\n object.nested.value = 2\n values[index] = 3\n object.value += 1\n values[index] -= 1\n values .= incoming\n local_value = 3\n local_value += 1\nend\n";
    let result = output(source);
    assert!(result.document().entities.iter().any(|entity| {
        entity.canonical_name == "local_value" && entity.kind == EntityKind::Variable
    }));
    assert!(
        !result.document().entities.iter().any(|entity| {
            entity.canonical_name == "value" && entity.kind == EntityKind::Variable
        })
    );
    assert_eq!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == FactDomain::Relations
                    && gap.detail == "julia-mutation-target-unavailable"
            })
            .map(|gap| {
                let span = gap.source.span();
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]
            })
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "object.value = 1",
            "object.nested.value = 2",
            "values[index] = 3",
            "object.value += 1",
            "values[index] -= 1",
            "values .= incoming",
        ])
    );
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "julia-written-declaration-shape-unavailable" })
    );
    let local_update = source.find("local_value += 1").unwrap();
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Entities
            && gap.detail == "julia-pattern-and-local-scope-bindings-unavailable"
            && gap.source.span().start_byte() == u64::try_from(local_update).unwrap()
    }));
}

#[test]
fn julia_tuple_patterns_keep_each_written_binding_and_exclude_value_names() {
    // Tuple assignments and discarded slots occur throughout Julia's compiler passes.
    let source = "function split_pair((left::Int, (right, tail...))::Tuple, ::Type{Int})\n (consistent, removable, nothrow) = flags(input)\n s, e = first(range), last(range)\n (_, sparams) = intersect(signature)\n (kept, object.property, values[index]) = incoming\n (; named_left, named_right) = named_input\n values = (value_left, value_right)\n return left\nend\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let function = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "split_pair")
        .unwrap();
    for (name, kind) in [
        ("left", EntityKind::Parameter),
        ("right", EntityKind::Parameter),
        ("tail", EntityKind::Parameter),
        ("consistent", EntityKind::Variable),
        ("removable", EntityKind::Variable),
        ("nothrow", EntityKind::Variable),
        ("s", EntityKind::Variable),
        ("e", EntityKind::Variable),
        ("sparams", EntityKind::Variable),
        ("kept", EntityKind::Variable),
        ("named_left", EntityKind::Variable),
        ("named_right", EntityKind::Variable),
    ] {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "missing or duplicate {name}: {:?}",
            document.entities
        );
        let entity = entities[0];
        let definitions: Vec<_> = document.occurrences.iter().filter(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == entity.id)).collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(function.id)
                    && relation.object == RelationEndpoint::Entity(entity.id))
        );
    }
    for forbidden in [
        "_",
        "Int",
        "property",
        "object",
        "index",
        "value_left",
        "value_right",
        "input",
        "named_input",
    ] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == forbidden),
            "invented {forbidden}"
        );
    }
}

#[test]
fn julia_tuple_block_expressions_do_not_invent_binding_names() {
    let result = output("(kept, (not_binding; hidden)) = incoming\n");
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.canonical_name.as_str(), "not_binding" | "hidden"))
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "julia-written-declaration-shape-unavailable")
    );
}

// Source-grounded loop/local headers establish written names without resolving uses.
// Explicit outer reuse and quoted source must not introduce new named declarations.
#[test]
fn julia_loop_local_and_let_headers_preserve_written_bindings() {
    let source = "function walk(items)\n local declared, typed::Int\n for loop_item in items\n end\n for (key, val) in items\n end\n let fresh, bound = 1\n fresh\n end\n reused = 0\n for outer reused = 1:2\n end\nend\nquote\n for quoted_name in items\n end\nend\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let function = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "walk")
        .unwrap();
    for name in [
        "declared",
        "typed",
        "loop_item",
        "key",
        "val",
        "fresh",
        "bound",
    ] {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == EntityKind::Variable)
            .collect();
        assert_eq!(entities.len(), 1, "missing or duplicate {name}");
        let entity = entities[0];
        let definitions: Vec<_> = document.occurrences.iter().filter(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == entity.id)).collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(function.id)
                    && relation.object == RelationEndpoint::Entity(entity.id))
        );
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| matches!(entity.canonical_name.as_str(), "Int" | "quoted_name"))
    );
    let outer = u64::try_from(source.find("outer reused").unwrap() + "outer ".len()).unwrap();
    assert!(
        !document
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == outer)
    );
}

#[test]
fn julia_short_callable_objects_keep_anonymous_owners_and_written_parameters() {
    let source = "struct Callable\n (receiver::Callable)(value) = value\n (::Callable)(other) = other\n function (long_receiver::Callable)(long_value)\n long_value\n end\nend\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owners: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.canonical_name == "<anonymous>" && entity.kind == EntityKind::Function
        })
        .collect();
    assert_eq!(owners.len(), 3);
    assert!(
        owners
            .iter()
            .all(|owner| owner.kind == EntityKind::Function)
    );
    for name in ["receiver", "value", "other", "long_receiver", "long_value"] {
        let parameter = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == EntityKind::Parameter)
            .unwrap_or_else(|| panic!("missing {name}"));
        let definition = document.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == parameter.id)).unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(parameter.id)
                    && owners
                        .iter()
                        .any(|owner| relation.subject == RelationEndpoint::Entity(owner.id)))
                .count(),
            1
        );
    }
    for owner in owners {
        assert!(!document.occurrences.iter().any(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == owner.id)));
    }
}

#[test]
fn julia_parametric_constructor_heads_preserve_names_and_parameters() {
    let source = "VecElement{T}(arg) where {T} = convert(T, arg)\nfunction Base.Array{T}(size::Int) where T\n size\nend\nobject.property = 1\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, parameter_name) in [("VecElement", "arg"), ("Array", "size")] {
        let function = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == EntityKind::Function)
            .unwrap_or_else(|| panic!("missing {name}"));
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == parameter_name && entity.kind == EntityKind::Parameter
            })
            .unwrap_or_else(|| panic!("missing {parameter_name}"));
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(function.id)
                    && relation.object == RelationEndpoint::Entity(parameter.id))
        );
        for entity in [function, parameter] {
            let definition = document.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == entity.id)).unwrap();
            let span = definition.source.span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                entity.canonical_name
            );
        }
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "property")
    );
}

#[test]
fn julia_type_and_method_parameters_keep_declared_owners_and_exact_names() {
    let source = "struct Box{T<:Number,N} <: AbstractArray{T,N}\n value::T\nend\nabstract type Family{U} end\nprimitive type Atom{V} 8 end\nf(x::F) where {F<:Number,S>:Int,K} = x\ng(x::G) where G where H = x\nconst Alias = Vector{Z} where Z\nh(x::W) where {Int<:W<:Real} = x\nr(x::R) where {Number>:R>:Int} = x\nquote\n struct Hidden{Q} end\nend\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, owner_name) in [
        ("T", "Box"),
        ("N", "Box"),
        ("U", "Family"),
        ("V", "Atom"),
        ("F", "f"),
        ("S", "f"),
        ("K", "f"),
        ("G", "g"),
        ("H", "g"),
        ("W", "h"),
        ("R", "r"),
    ] {
        let parameters: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.canonical_name == name && entity.kind == EntityKind::TypeParameter
            })
            .collect();
        assert_eq!(parameters.len(), 1, "missing or duplicate {name}");
        let parameter = parameters[0];
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner_name)
            .unwrap();
        let definitions: Vec<_> = document.occurrences.iter().filter(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == parameter.id)).collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(parameter.id))
        );
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::TypeParameter)
            .count(),
        11
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "julia-type-expression-binding-scope-unavailable")
    );
}

#[test]
fn julia_anonymous_functions_preserve_parameters_without_invented_names() {
    let source = "single = scalar -> scalar\npair = (left, right::Int) -> left\ntupled = ((first, second), tail...) -> first\nlong = function (entry::T) where T\n entry\nend\nmulti = function (one, two)\n one\nend\nempty = function ()\n nothing\nend\ntyped = (value::Int)::Int -> value\nmap(items) do row, column\n row\nend\nmap(items) do (alpha, beta), rest\n alpha\nend\nmap(items) do\n nothing\nend\nquote\n hidden -> hidden\n function (quoted)\n quoted\n end\nend\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owners: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "<anonymous>")
        .collect();
    assert_eq!(
        owners.len(),
        10,
        "{:?}",
        document
            .entities
            .iter()
            .map(|entity| (&entity.canonical_name, entity.kind))
            .collect::<Vec<_>>()
    );
    assert!(
        owners
            .iter()
            .all(|owner| owner.kind == EntityKind::Function)
    );
    for name in [
        "scalar", "left", "right", "first", "second", "tail", "entry", "one", "two", "value",
        "row", "column", "alpha", "beta", "rest",
    ] {
        let parameters: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(parameters.len(), 1, "{name}: {parameters:?}");
        let parameter = parameters[0];
        assert_eq!(parameter.kind, EntityKind::Parameter, "{name}");
        let definition = document.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == parameter.id)).unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(parameter.id)
                    && owners
                        .iter()
                        .any(|owner| relation.subject == RelationEndpoint::Entity(owner.id)))
                .count(),
            1,
            "{name}"
        );
    }
    for owner in owners {
        assert!(!document.occurrences.iter().any(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == owner.id)));
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| matches!(entity.canonical_name.as_str(), "hidden" | "quoted"))
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "T" && entity.kind == EntityKind::TypeParameter)
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "julia-anonymous-function-binding-unavailable")
    );
}

#[test]
fn julia_unnamed_typed_parameters_keep_source_sites_without_definitions() {
    let source = "take(::Int, ::Int, named) = named\nstruct Callable\n (::Callable)(::Vector{T}, value::T) where T = value\nend\nclosure = (::Float64, other) -> other\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let parameters: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Parameter && entity.canonical_name == "<anonymous>"
        })
        .collect();
    assert_eq!(parameters.len(), 5, "{:?}", document.entities);
    let sites: BTreeSet<_> = parameters
        .iter()
        .map(|entity| {
            let span = entity.evidence.source.as_ref().unwrap().span();
            (span.start_byte(), span.end_byte())
        })
        .collect();
    assert_eq!(sites.len(), 5);
    for (text, parent_name) in [
        ("::Int", "take"),
        ("::Callable", "<anonymous>"),
        ("::Vector{T}", "<anonymous>"),
        ("::Float64", "<anonymous>"),
    ] {
        for (offset, _) in source.match_indices(text) {
            let parameter = parameters
                .iter()
                .find(|entity| {
                    let span = entity.evidence.source.as_ref().unwrap().span();
                    span.start_byte() == u64::try_from(offset).unwrap()
                        && span.end_byte() == u64::try_from(offset + text.len()).unwrap()
                })
                .unwrap_or_else(|| panic!("missing {text} at {offset}"));
            assert!(!document.occurrences.iter().any(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved {symbol} if symbol == parameter.id)));
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|relation| relation.predicate == RelationPredicate::Contains
                        && relation.object == RelationEndpoint::Entity(parameter.id)
                        && document
                            .entities
                            .iter()
                            .any(|owner| owner.canonical_name == parent_name
                                && owner.kind == EntityKind::Function
                                && relation.subject == RelationEndpoint::Entity(owner.id)))
                    .count(),
                1
            );
        }
    }
    for name in ["Int", "Vector", "Float64"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
}

#[test]
fn julia_empty_and_control_identifiers_keep_decoded_names_and_authored_displays() {
    let source = "var\"\" = 1\nvar\"\" = 2\nvar\"line\nname\" = 3\nvar\"tab\tname\" = 4\ntail = 5\nvar\"line\nname\"()\n";
    let result = output(source);
    let document = result.document();
    assert!(document.diagnostics.is_empty());
    let empty: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name.is_empty())
        .collect();
    assert_eq!(empty.len(), 2);
    assert_ne!(empty[0].id, empty[1].id);
    for (written, canonical, count) in [
        ("var\"\"", "", 2),
        ("var\"line\nname\"", "line\nname", 1),
        ("var\"tab\tname\"", "tab\tname", 1),
    ] {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                let span = occurrence.source.span();
                occurrence.role == OccurrenceRole::Definition
                    && &source[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()]
                        == written
            })
            .collect();
        assert_eq!(definitions.len(), count);
        for definition in definitions {
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("quoted definition unresolved")
            };
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!(entity.canonical_name, canonical);
            assert_eq!(entity.display_name, written);
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash(written.as_bytes())
            );
            assert!(
                document
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Contains
                        && relation.object == RelationEndpoint::Entity(symbol))
            );
        }
    }
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "tail" && entity.display_name == "tail")
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "declaration-name-unavailable")
    );
}

#[test]
fn julia_empty_and_control_name_evidence_stays_on_original_encoded_bytes() {
    let text = "# λ😀\nvar\"\" = 1\nvar\"line\nname\" = 2\nvar\"tab\tname\" = 3\ntail = 4\n";
    let mut utf8_bom = vec![0xef, 0xbb, 0xbf];
    utf8_bom.extend_from_slice(text.as_bytes());
    let mut utf16_le = vec![0xff, 0xfe];
    let mut utf16_be = vec![0xfe, 0xff];
    for unit in text.encode_utf16() {
        utf16_le.extend_from_slice(&unit.to_le_bytes());
        utf16_be.extend_from_slice(&unit.to_be_bytes());
    }
    for (bytes, bom, utf16) in [
        (text.as_bytes().to_vec(), 0, false),
        (utf8_bom, 3, false),
        (utf16_le, 2, true),
        (utf16_be, 2, true),
    ] {
        let provider = Arc::new(provider());
        let fixture = Fixture::new(JULIA, &bytes);
        let budget = limits();
        let result = analyze(
            &analyzer(&provider, JULIA),
            &request(&fixture.snapshot, &fixture.source, JULIA, &budget),
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        for (written, canonical) in [
            ("var\"\"", ""),
            ("var\"line\nname\"", "line\nname"),
            ("var\"tab\tname\"", "tab\tname"),
        ] {
            let logical_start = text.find(written).unwrap();
            let physical_start = bom
                + if utf16 {
                    text[..logical_start].encode_utf16().count() * 2
                } else {
                    logical_start
                };
            let physical_length = if utf16 {
                written.encode_utf16().count() * 2
            } else {
                written.len()
            };
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.source.span().start_byte()
                            == u64::try_from(physical_start).unwrap()
                        && occurrence.source.span().end_byte()
                            == u64::try_from(physical_start + physical_length).unwrap()
                })
                .collect();
            assert_eq!(definitions.len(), 1, "{written:?}");
            let definition = definitions[0];
            assert_eq!(definition.source.content_hash(), content_hash(&bytes));
            let physical = &bytes[physical_start..physical_start + physical_length];
            let expected_physical = if utf16 {
                written
                    .encode_utf16()
                    .flat_map(|unit| {
                        if bytes.starts_with(&[0xff, 0xfe]) {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                written.as_bytes().to_vec()
            };
            assert_eq!(physical, expected_physical);
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash(written.as_bytes())
            );
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("encoded quoted definition unresolved")
            };
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!(entity.canonical_name, canonical);
            assert_eq!(entity.display_name, written);
        }
    }
}

#[test]
fn julia_quoted_name_preflight_artifact_replay_and_cancellation_agree() {
    let source = "var\"\" = 1\nvar\"line\nname\" = 2\nvar\"tab\tname\" = 3\ntail = 4\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, JULIA);
    let fixture = Fixture::new(JULIA, source.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, JULIA, &budget);
    let required = provider
        .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
        .unwrap();
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        required,
        artifact
            .required_syntax_fact_count(&Cancellation::new())
            .unwrap()
    );
    let replay = analyzer
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    for canonical in ["", "line\nname", "tab\tname"] {
        assert!(
            fresh
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == canonical)
        );
    }
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(
        provider
            .required_syntax_fact_count(&input.to_parse_request(), &cancellation)
            .is_err()
    );
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn julia_unrepresentable_authored_display_cannot_promote_owned_parameters() {
    let written = format!("var\"{}\"\n\"", "\\".repeat(101));
    let source = format!("function {written}(child)\n child\nend\ntail = 1\n");
    let provider = Arc::new(provider());
    let fixture = Fixture::new(JULIA, source.as_bytes());
    let defaults = limits();
    let mut ir = defaults.ir().clone();
    ir.max_string_bytes = 80;
    let budget = AnalysisLimits::new(
        defaults.max_source_bytes(),
        defaults.max_syntax_nodes(),
        defaults.max_syntax_depth(),
        defaults.max_embedded_ranges(),
        defaults.max_reported_memory_bytes(),
        defaults.syntax_stream().clone(),
        defaults.ir_stream().clone(),
        ir,
    )
    .unwrap();
    let result = analyze(
        &analyzer(&provider, JULIA),
        &request(&fixture.snapshot, &fixture.source, JULIA, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "tail")
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "child")
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Function)
    );
    let owner_end = u64::try_from(source.find("\nend").unwrap() + 4).unwrap();
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.domain == rootlight_ir::FactDomain::Entities
            && gap.detail == "declaration-name-unavailable"
            && gap.source.span().start_byte() == 0
            && gap.source.span().end_byte() == owner_end
    }));
}
