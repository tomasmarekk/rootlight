//! PowerShell declarations and references through the production analyzer.
//! Source evidence must remain exact without claiming runtime command resolution.

use super::*;

const POWERSHELL: LanguageCase = LanguageCase {
    name: "powershell",
    path: "src/catalog.psm1",
    frontend: "tree-sitter-powershell-0.26.4",
    source: "function Read-Entry { return 1 }",
    generated: false,
    body_before: "return 1",
    body_after: "return 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(POWERSHELL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, POWERSHELL),
        &request(&fixture.snapshot, &fixture.source, POWERSHELL, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn powershell_written_declarations_have_exact_definitions_and_owners() {
    let source = "function script:Read-Entry { param([string]$Name); return $Name }\nclass Cache { [string]$Label; Cache([string]$name) { $this.Label = $name } [string] Read([int]$slot) { return $this.Label } }\nenum Mode { Open = 1; Closed = 2 }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind) in [
        ("script:Read-Entry", EntityKind::Function),
        ("$Name", EntityKind::Parameter),
        ("Cache", EntityKind::Class),
        ("Cache", EntityKind::Constructor),
        ("$Label", EntityKind::Field),
        ("Read", EntityKind::Method),
        ("$slot", EntityKind::Parameter),
        ("Mode", EntityKind::Enum),
        ("Open", EntityKind::Constant),
        ("Closed", EntityKind::Constant),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|e| e.canonical_name == name && e.kind == kind)
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "{name}/{kind:?}: {:?}; gaps: {:?}",
            doc.entities,
            doc.skipped_regions
        );
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target
                        == (OccurrenceTarget::Resolved {
                            symbol: entities[0].id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
    }
    for (child, parent) in [
        ("$Name", "script:Read-Entry"),
        ("$slot", "Read"),
        ("$Label", "Cache"),
    ] {
        let entity = doc
            .entities
            .iter()
            .find(|e| e.canonical_name == child)
            .unwrap();
        let owner = doc
            .entities
            .iter()
            .find(|e| Some(rootlight_ir::ContainerRef::Entity(e.id)) == entity.container)
            .unwrap();
        assert_eq!(owner.canonical_name, parent);
    }
    assert!(
        doc.skipped_regions.iter().any(|gap| gap.detail
            == "powershell-runtime-command-module-and-dispatch-resolution-unavailable")
    );
}

#[test]
fn powershell_body_only_edits_preserve_declaration_identity() {
    let source = "function Read-Entry { param([string]$Name, [int]$Count = 1); return $Name }\nclass Cache { [int] Read([int]$slot) { return $slot } }\n";
    let initial = output(source);
    let changed = output(
        &source
            .replace("return $Name", "Write-Output '雪'; return $Name")
            .replace("return $slot", "return $slot + 1"),
    );
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|e| (e.id, (e.kind, e.canonical_name.clone(), e.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&initial), identities(&changed));
}

#[test]
fn powershell_assignments_keep_written_names_without_inventing_member_bindings() {
    let source = "$value = 1\n${name with space} = '雪'\nforeach ($item in 1,2) { Write-Output $item }\n$object.Field = 3\n$array[0] = 4\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let names: BTreeSet<_> = doc
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Variable)
        .map(|e| e.canonical_name.as_str())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(["$value", "${name with space}", "$item"])
    );
    assert!(doc.occurrences.iter().any(|o| {
        let span = o.source.span();
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap(),
        ) == Some("$object")
    }));
}

#[test]
fn powershell_calls_and_literal_boundaries_preserve_source_evidence() {
    let source = "function Read-Entry { return 1 }\n# function Hidden {}\n$text = @'\nclass Hidden {}\n'@\nRead-Entry | Write-Output\n$object.Read(1)\n& $action\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert!(!doc.entities.iter().any(|e| e.canonical_name == "Hidden"));
    for name in ["Read-Entry", "Write-Output", "Read"] {
        assert!(
            doc.occurrences.iter().any(|o| {
                let span = o.source.span();
                o.role != OccurrenceRole::Definition
                    && source.get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    ) == Some(name)
            }),
            "{name}: {:?}",
            doc.occurrences
                .iter()
                .map(|o| (
                    o.role,
                    source.get(
                        usize::try_from(o.source.span().start_byte()).unwrap()
                            ..usize::try_from(o.source.span().end_byte()).unwrap()
                    )
                ))
                .collect::<Vec<_>>()
        );
    }
    assert!(
        doc.skipped_regions.iter().any(|gap| gap.detail
            == "powershell-runtime-command-module-and-dispatch-resolution-unavailable")
    );
}

#[test]
fn powershell_repeated_written_declarations_keep_distinct_source_identities() {
    let source = "$value = 1\n$value = 2\nfunction Read-Entry { return 1 }\nfunction Read-Entry { return 2 }\n";
    let result = output(source);
    let changed = output(
        &source
            .replace("= 1", "= 100")
            .replace("return 2", "return 200"),
    );
    for name in ["$value", "Read-Entry"] {
        let identities = |result: &AnalysisOutput| {
            result
                .document()
                .entities
                .iter()
                .filter(|e| e.canonical_name == name)
                .map(|e| e.id)
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            identities(&result).len(),
            2,
            "{name}: {:?}",
            result.document().skipped_regions
        );
        assert_eq!(identities(&result), identities(&changed));
    }
}

#[test]
fn powershell_unmodeled_data_members_remain_explicit_source_scoped_gaps() {
    let source = "@{ ModuleVersion = '1.0.0'; RootModule = 'catalog.psm1' }\n";
    let result = output(source);
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "powershell-data-member-analysis-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    let span = gaps[0].source.span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some(source.trim_end())
    );
    assert_eq!(gaps[0].domain, FactDomain::Entities);
}
