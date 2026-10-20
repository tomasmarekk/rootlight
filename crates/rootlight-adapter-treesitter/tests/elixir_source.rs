//! Elixir source contracts retain written owners without compiler or macro claims.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, IrLimits, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/elixir/gzip.ex");

fn analyze_at(path: &str, text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    let directory = tempdir().unwrap();
    let file = directory.path().join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
    let repository = derive_repository(b"elixir-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 1024 * 1024)
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = TreeSitterProvider::new(
        RuntimeConfig::new(
            1024 * 1024,
            16_384,
            128,
            16,
            64,
            1,
            16 * 1024 * 1024,
            ParserSettings::new(256).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "elixir-source-contract",
            "1.0",
            content_hash(b"elixir-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("elixir").unwrap(),
        "elixir-static-contract",
        content_hash(b"elixir-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("elixir").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"elixir-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )
    .unwrap();
    let document = output.document();
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].language, "elixir");
    assert_eq!(
        document.files[0].content_hash,
        content_hash(text.as_bytes())
    );
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(text.len()).unwrap()
    );
    for occurrence in &document.occurrences {
        assert_eq!(occurrence.source.repository(), repository);
        assert_eq!(occurrence.source.generation(), source.generation());
        assert_eq!(occurrence.source.span().file(), source.span().file());
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(text.as_bytes())
        );
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(spelling(text, &occurrence.source).as_bytes())
        );
    }
    for gap in &document.skipped_regions {
        assert_eq!(gap.source.content_hash(), content_hash(text.as_bytes()));
        let _ = spelling(text, &gap.source);
    }
    output
}

fn spelling<'a>(text: &'a str, source: &SourceRef) -> &'a str {
    text.get(
        usize::try_from(source.span().start_byte()).unwrap()
            ..usize::try_from(source.span().end_byte()).unwrap(),
    )
    .unwrap()
}

fn entity<'a>(
    output: &'a rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let mut found = output
        .document()
        .entities
        .iter()
        .filter(|record| record.canonical_name == name && record.kind == kind);
    let result = found
        .next()
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"));
    assert!(found.next().is_none(), "ambiguous fixture {name}");
    result
}

fn definition(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    text: &str,
    name: &str,
    kind: EntityKind,
    marker: &str,
) {
    let owner = entity(output, name, kind);
    let found = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: owner.id }
        })
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1, "{name}");
    let start = text.find(marker).unwrap() + marker.find(name).unwrap();
    assert_eq!(
        found[0].source.span().start_byte(),
        u64::try_from(start).unwrap()
    );
    assert_eq!(
        found[0].source.span().end_byte(),
        u64::try_from(start + name.len()).unwrap()
    );
    assert_eq!(spelling(text, &found[0].source), name);
}

fn contains(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    parent: &str,
    parent_kind: EntityKind,
    child: &str,
    witness: (u64, u64),
) {
    let parent = entity(output, parent, parent_kind);
    let child = entity(output, child, EntityKind::Function);
    assert_eq!(
        child.container,
        Some(rootlight_ir::ContainerRef::Entity(parent.id))
    );
    let edges = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent.id)
                && relation.object == RelationEndpoint::Entity(child.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    let source = edges[0].evidence.source.as_ref().unwrap();
    assert!(source.span().start_byte() <= witness.0 && source.span().end_byte() >= witness.1);
    assert_eq!(source.repository(), child.repository);
    assert_eq!(source.generation(), child.generation);
}

#[test]
fn phoenix_finite_obligations_keep_exact_names_and_both_contains_witnesses() {
    assert_eq!(SOURCE.len(), 406);
    let output = analyze_at("lib/phoenix/digester/gzip.ex", SOURCE);
    for (name, kind, marker) in [
        (
            "Phoenix.Digester.Gzip",
            EntityKind::Module,
            "defmodule Phoenix.Digester.Gzip",
        ),
        ("compress_file", EntityKind::Function, "def compress_file"),
        (
            "file_extensions",
            EntityKind::Function,
            "def file_extensions",
        ),
    ] {
        definition(&output, SOURCE, name, kind, marker);
    }
    contains(
        &output,
        "Phoenix.Digester.Gzip",
        EntityKind::Module,
        "compress_file",
        (153, 166),
    );
    contains(
        &output,
        "Phoenix.Digester.Gzip",
        EntityKind::Module,
        "file_extensions",
        (361, 376),
    );
    assert!(!output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span().start_byte() < 94
            && occurrence.source.span().end_byte() > 78
    }));
}

#[test]
fn guarded_private_zero_arity_and_short_body_functions_preserve_source_names() {
    let source = "# 🌐\r\ndefmodule Sample do\r\n  def ready?(value) when is_integer(value), do: value > 0\r\n  defp encode!(value) do\r\n    value\r\n  end\r\n  def count, do: 3\r\nend\r\n";
    let output = analyze_at("sample.exs", source);
    for (name, marker) in [
        ("ready?", "def ready?"),
        ("encode!", "defp encode!"),
        ("count", "def count"),
    ] {
        definition(&output, source, name, EntityKind::Function, marker);
    }
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "is_integer")
    );
}

#[test]
fn nested_modules_own_only_their_direct_written_functions() {
    let source = "defmodule Outer do\n def first, do: 1\n defmodule Inner do\n  def second, do: 2\n end\nend\n";
    let output = analyze_at("nested.ex", source);
    let first = u64::try_from(source.find("first").unwrap()).unwrap();
    let second = u64::try_from(source.find("second").unwrap()).unwrap();
    contains(
        &output,
        "Outer",
        EntityKind::Module,
        "first",
        (first, first + 5),
    );
    contains(
        &output,
        "Inner",
        EntityKind::Module,
        "second",
        (second, second + 6),
    );
    let outer = entity(&output, "Outer", EntityKind::Module);
    let inner = entity(&output, "Inner", EntityKind::Module);
    assert_eq!(
        inner.container,
        Some(rootlight_ir::ContainerRef::Entity(outer.id))
    );
}

#[test]
fn quotes_documentation_and_comments_cannot_publish_declarations() {
    let source = "defmodule Written do\n @moduledoc \"defmodule InString do end\"\n # def fake, do: 0\n quote do\n  def ghost, do: 1\n end\n def live, do: :ok\nend\n";
    let output = analyze_at("quoted.ex", source);
    definition(&output, source, "live", EntityKind::Function, "def live");
    for absent in ["InString", "fake", "ghost"] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == absent),
            "{absent}"
        );
    }
    assert!(!output.document().skipped_regions.is_empty());
}

#[test]
fn malformed_or_dynamic_module_owners_do_not_fabricate_owned_functions() {
    for source in [
        "defmodule Dynamic.name() do\n def hidden, do: 1\nend\n",
        "defmodule Broken do\n def hidden(value do\n value\n end\n",
        "quote do: (def hidden, do: :ok)\n",
        "defmodule Written do\n Kernel.quote do\n def hidden, do: :ok\n end\nend\n",
        "defmodule Written do\n callback = fn -> def hidden, do: :ok end\nend\n",
    ] {
        let output = analyze_at("unsupported.ex", source);
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "hidden")
        );
        assert!(!output.document().skipped_regions.is_empty());
    }
}

#[test]
fn same_spelling_function_clauses_have_distinct_source_identities() {
    let source = "defmodule Multi do\n def value(0), do: :zero\n def value(n), do: n\nend\n";
    let output = analyze_at("multi.ex", source);
    let functions = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "value")
        .collect::<Vec<_>>();
    assert_eq!(
        functions.len(),
        2,
        "entities={:#?}; gaps={:#?}",
        output.document().entities,
        output.document().skipped_regions
    );
    assert_ne!(functions[0].id, functions[1].id);
    let definitions = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && spelling(source, &occurrence.source) == "value"
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 2);
    assert_ne!(definitions[0].source.span(), definitions[1].source.span());
}

#[test]
fn dynamic_function_heads_remain_gaps_without_hiding_literal_siblings() {
    for head in [
        "unquote(name)",
        "unquote_splicing(names)",
        "unquote(name) when is_atom(name)",
    ] {
        let source = format!(
            "defmodule Sample do\n name = :chosen\n def {head}, do: :ok\n def live, do: :ok\n def stable(unquote(args)), do: :ok\nend\n"
        );
        let output = analyze_at("dynamic_head.ex", &source);
        definition(&output, &source, "live", EntityKind::Function, "def live");
        definition(
            &output,
            &source,
            "stable",
            EntityKind::Function,
            "def stable",
        );
        assert!(
            !output.document().entities.iter().any(|entity| {
                entity.kind == EntityKind::Function
                    && matches!(
                        entity.canonical_name.as_str(),
                        "unquote" | "unquote_splicing"
                    )
            }),
            "dynamic head {head} must not become a literal function"
        );
        assert!(output.document().skipped_regions.iter().any(|gap| {
            gap.detail == "elixir-unmodeled-dynamic-quoted-or-recovered-construct"
                && spelling(&source, &gap.source).contains(head)
        }));
    }
}

#[test]
fn written_structure_never_claims_macro_expansion_or_resolved_call_binding() {
    let source = "defmodule Partial do\n def live(value), do: external(value)\n defmacro generated(), do: quote(do: :ok)\nend\n";
    let output = analyze_at("partial.ex", source);
    definition(&output, source, "live", EntityKind::Function, "def live");
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "generated")
    );
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    for reason in [
        "elixir-macro-expansion-import-arity-type-and-runtime-binding-unavailable",
        "elixir-parameter-pattern-attribute-generated-and-macro-declarations-unavailable",
        "elixir-variable-call-arity-and-macro-occurrence-binding-unavailable",
    ] {
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == reason),
            "{reason}; observed gaps={:#?}",
            output.document().skipped_regions
        );
    }
}
