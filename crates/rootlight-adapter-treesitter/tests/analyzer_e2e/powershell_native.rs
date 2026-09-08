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
fn powershell_nested_literals_preserve_complete_bounded_source_ownership() {
    let mut value = "{ param($leaf) $leaf }".to_owned();
    for _ in 0..8 {
        value = format!("@{{ Entry = {value} }}");
    }
    let source = format!("Invoke-Entry {{ $value = {value} }}\nfunction Visible {{ return 1 }}\n");
    let result = output(&source);
    let doc = result.document();
    assert!(
        !doc.skipped_regions.iter().any(|gap| matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::ResourceLimit
        )),
        "{:?}",
        doc.skipped_regions
    );
    assert_eq!(
        doc.entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Property)
            .count(),
        8
    );
    assert_eq!(
        doc.entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        3
    );
    let leaf = doc
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "$leaf")
        .unwrap();
    let owner = doc
        .entities
        .iter()
        .find(|entity| leaf.container == Some(rootlight_ir::ContainerRef::Entity(entity.id)))
        .unwrap();
    assert!(owner.flags.contains(&EntityFlag::Synthetic));
    let span = owner.evidence.source.as_ref().unwrap().span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some("{ param($leaf) $leaf }")
    );
    assert!(
        doc.relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(leaf.id))
    );
}

#[test]
fn powershell_anonymous_blocks_own_parameters_and_nested_callables() {
    let source = "function Outer { param($root); Invoke-Entry { param($value) { param($inner) $inner + $value } } }\n";
    let result = output(source);
    let doc = result.document();
    let functions: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect();
    assert_eq!(functions.len(), 3, "source callables need distinct owners");
    for (name, full) in [
        ("$root", source.trim()),
        (
            "$value",
            "{ param($value) { param($inner) $inner + $value } }",
        ),
        ("$inner", "{ param($inner) $inner + $value }"),
    ] {
        let parameter = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let owner = functions
            .iter()
            .find(|entity| {
                parameter.container == Some(rootlight_ir::ContainerRef::Entity(entity.id))
            })
            .unwrap();
        let span = owner.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(full)
        );
        if name != "$root" {
            assert!(owner.flags.contains(&rootlight_ir::EntityFlag::Synthetic));
            assert!(
                !doc.occurrences
                    .iter()
                    .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: owner.id })
            );
        }
        assert!(
            doc.relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(parameter.id))
        );
    }
    assert!(
        !doc.skipped_regions
            .iter()
            .any(|gap| gap.detail == "powershell-runtime-script-block-identity-unavailable")
    );
    assert!(
        doc.skipped_regions.iter().any(|gap| gap.detail
            == "powershell-runtime-command-module-and-dispatch-resolution-unavailable")
    );
}

#[test]
fn powershell_anonymous_headers_and_source_owners_cover_expression_positions() {
    for (source, header, full) in [
        ("{}", "{", "{}"),
        ("{<# empty #>}", "{", "{<# empty #>}"),
        (
            "$handler = { param($value) $value }",
            "{ param($value)",
            "{ param($value) $value }",
        ),
        (
            "& { param($value) $value } 'entry'",
            "{ param($value)",
            "{ param($value) $value }",
        ),
        (
            "$items.Where{ param($value) $value }",
            "{ param($value)",
            "{ param($value) $value }",
        ),
        (
            "@{ Key = { param($value) $value } }",
            "{ param($value)",
            "{ param($value) $value }",
        ),
        (
            "function Outer { param($callback = { param($value) $value }); $callback }",
            "{ param($value)",
            "{ param($value) $value }",
        ),
    ] {
        let result = output(source);
        let doc = result.document();
        assert!(doc.diagnostics.is_empty());
        assert!(
            !doc.skipped_regions.iter().any(|gap| matches!(
                gap.reason,
                SkippedRegionReason::ParseError | SkippedRegionReason::ResourceLimit
            )),
            "{source}"
        );
        let anonymous: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Function && entity.flags.contains(&EntityFlag::Synthetic)
            })
            .collect();
        assert_eq!(anonymous.len(), 1, "{source}: {:?}", doc.entities);
        let entity = anonymous[0];
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(full)
        );
        let signatures: Vec<_> = doc
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && lexical.subject() == rootlight_ir::FactRef::Entity(entity.id))
                .then(|| lexical.text().to_owned())
            })
            .collect();
        assert_eq!(signatures, [header], "{source}");
        assert!(
            !doc.occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id })
        );
        if header.contains("$value") {
            let parameter = doc
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "$value")
                .unwrap();
            assert_eq!(
                parameter.container,
                Some(rootlight_ir::ContainerRef::Entity(entity.id))
            );
        }
    }
}

#[test]
fn powershell_anonymous_siblings_preserve_identity_across_body_and_offset_edits() {
    let source = "Invoke-Entry { param($value) $value + 1 } { param($value) $value + 2 }\n";
    let initial = output(source);
    let changed = output(
        &source
            .replace("Invoke-Entry", "# prefix\nInvoke-Entry")
            .replace("+ 1", "+ 1000")
            .replace("+ 2", "+ 2000"),
    );
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, (entity.canonical_name.clone(), entity.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&initial), identities(&changed));
    let parameters: Vec<_> = initial
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "$value")
        .collect();
    assert_eq!(parameters.len(), 2);
    assert_ne!(parameters[0].id, parameters[1].id);
    assert_ne!(parameters[0].container, parameters[1].container);
}

#[test]
fn powershell_string_hashes_keep_nested_evidence_and_following_definitions() {
    let source = "$text = \"`r`n## Heading`r`n\"\nInvoke-Entry name=\"$value# function Hidden {}\"\n$text = @\"\n$other# function Hidden {}\n\"@\n$text = \"$(<# $ignored #> Read-Value)# literal\" # $outside\nfunction Visible { return $text }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert!(
        !doc.skipped_regions.iter().any(|gap| matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::ResourceLimit
        )),
        "{:?}",
        doc.skipped_regions
    );
    let functions: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(functions, ["Visible"]);
    for name in ["$value", "$other", "Read-Value"] {
        let hash = content_hash(name.as_bytes());
        assert!(
            doc.occurrences.iter().any(|occurrence| {
                let span = occurrence.source.span();
                occurrence.syntactic_text_hash == hash
                    && source.get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    ) == Some(name)
            }),
            "missing source evidence for {name}"
        );
    }
    for name in ["$ignored", "$outside", "Hidden"] {
        let hash = content_hash(name.as_bytes());
        assert!(
            !doc.occurrences
                .iter()
                .any(|occurrence| occurrence.syntactic_text_hash == hash),
            "{name}"
        );
    }
}

#[test]
fn powershell_composite_arguments_preserve_nested_calls_and_variable_evidence() {
    let source = "Invoke-Entry name=\"$($name)\" $env:root\\Cache\\Data pre$(Read-Value)post label='function Hidden {}'\nWrite-Output \"$($items | Select-Entry Name, Description | Out-String)\"\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert!(
        !doc.skipped_regions.iter().any(|gap| matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::ResourceLimit
        )),
        "{:?}",
        doc.skipped_regions
    );
    assert!(
        !doc.entities
            .iter()
            .any(|entity| entity.canonical_name == "Hidden")
    );
    let calls: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 5, "{calls:?}");
    for (name, full) in [
        ("Read-Value", "Read-Value"),
        // The native command span owns the separator before the next pipe.
        ("Select-Entry", "Select-Entry Name, Description "),
        ("Out-String", "Out-String"),
    ] {
        let hash = content_hash(name.as_bytes());
        let call = calls
            .iter()
            .find(|call| call.syntactic_text_hash == hash)
            .unwrap();
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(full)
        );
        assert_eq!(
            call.target,
            OccurrenceTarget::Unresolved { text_hash: hash }
        );
    }
    for name in ["$name", "$env:root", "$items"] {
        assert!(
            doc.occurrences.iter().any(|occurrence| {
                let span = occurrence.source.span();
                occurrence.role == OccurrenceRole::Reference
                    && source.get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    ) == Some(name)
            }),
            "{name}"
        );
    }
}

#[test]
fn powershell_script_block_method_calls_keep_exact_source_and_unresolved_names() {
    for (expression, member) in [
        ("$items.Where{ $_ }", "Where"),
        ("$items.ForEach{}", "ForEach"),
        ("($items.Keys).Apply{<# no action #>}", "Apply"),
        ("[Item]::Read{ param($entry); $entry }", "Read"),
    ] {
        let source = format!("$result = {expression}\n");
        let result = output(&source);
        let doc = result.document();
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        assert!(
            !doc.skipped_regions.iter().any(|gap| matches!(
                gap.reason,
                SkippedRegionReason::ParseError | SkippedRegionReason::ResourceLimit
            )),
            "{:?}",
            doc.skipped_regions
        );
        let calls: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .collect();
        assert_eq!(calls.len(), 1, "{source}: {calls:?}");
        let call = calls[0];
        let expected_hash = content_hash(member.as_bytes());
        assert_eq!(call.syntactic_text_hash, expected_hash);
        assert_eq!(
            call.target,
            OccurrenceTarget::Unresolved {
                text_hash: expected_hash
            }
        );
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(expression)
        );
        assert!(doc.skipped_regions.iter().any(|gap| gap.detail
            == "powershell-runtime-command-module-and-dispatch-resolution-unavailable"));
        assert!(doc.relations.iter().all(|relation| !matches!(
            relation.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        )));
    }
}

#[test]
fn powershell_numeric_literals_do_not_become_command_references() {
    let source =
        "$size = 2kB\n$rate = 2E+2MB\n$mask = 0X2LPb\nfunction Read-Size { return $size }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert!(
        !doc.skipped_regions.iter().any(|gap| matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::ResourceLimit
        )),
        "{:?}",
        doc.skipped_regions
    );
    for name in ["$size", "$rate", "$mask", "Read-Size"] {
        assert!(
            doc.entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}: {:?}",
            doc.entities
        );
    }
    for occurrence in &doc.occurrences {
        let span = occurrence.source.span();
        let text = source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert!(
            !matches!(text, "2kB" | "2E+2MB" | "0X2LPb"),
            "numeric literal became an occurrence: {occurrence:?}"
        );
    }
}

#[test]
fn powershell_empty_blocks_keep_definitions_without_false_parse_gaps() {
    for body in ["", " ", "\r\n", "<# no action #>", "# no action\n"] {
        let block = format!("{{{body}}}");
        let source = format!(
            "$action = {block}\nInvoke-Check Write-Entry {block}\nfunction Read-Entry {{ return 1 }}\n"
        );
        let result = output(&source);
        let doc = result.document();
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        assert!(
            !doc.skipped_regions.iter().any(|gap| matches!(
                gap.reason,
                SkippedRegionReason::ParseError | SkippedRegionReason::ResourceLimit
            )),
            "{:?}",
            doc.skipped_regions
        );
        for name in ["$action", "Read-Entry"] {
            let entity = doc
                .entities
                .iter()
                .find(|e| e.canonical_name == name)
                .unwrap();
            let definition = doc
                .occurrences
                .iter()
                .find(|o| {
                    o.role == OccurrenceRole::Definition
                        && o.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .unwrap();
            let span = definition.source.span();
            assert_eq!(
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(name)
            );
        }
        let blocks: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Function && entity.flags.contains(&EntityFlag::Synthetic)
            })
            .map(|entity| {
                let span = entity.evidence.source.as_ref().unwrap().span();
                source
                    .get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    )
                    .unwrap()
            })
            .collect();
        assert_eq!(blocks, vec![block.as_str(); 2]);
    }
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
fn powershell_typed_and_multiple_assignment_targets_keep_exact_definitions() {
    let source = "[int]$number = 1\n[string]${display name} = 'text'\n$first, $second = 1, 2\n$head, [int]$tail = 3, 4\n$object.Field, $array[$index], $last = 5, 6, 7\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let variables: BTreeMap<_, _> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .map(|entity| (entity.canonical_name.as_str(), entity.id))
        .collect();
    assert_eq!(
        variables.keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "$number",
            "${display name}",
            "$first",
            "$second",
            "$head",
            "$tail",
            "$last"
        ])
    );
    for (name, id) in variables {
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: id })
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
    let updated = output(
        &source
            .replace("= 1", "= 100")
            .replace("'text'", "'longer text'"),
    );
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| {
                (
                    entity.id,
                    (entity.kind, entity.canonical_name.clone(), entity.container),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&result), identities(&updated));
}

#[test]
fn powershell_assignment_reads_do_not_become_written_definitions() {
    let source = "$target = $read + $other\n$object[$index + $offset] = $value\n$object.Field, $array[$slot] = $left, $right\n[Console]::WriteLine($argument)\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    let names: BTreeSet<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names, BTreeSet::from(["$target"]));
}

#[test]
fn powershell_assignment_artifacts_preserve_all_targets_and_rebind_generation() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, POWERSHELL);
    let fixture = Fixture::new(
        POWERSHELL,
        b"function Read-Entry { [int]$first, $second = 1, 2; $map = @{ Key = 1; $key = @{ Inner = 2 } }; Invoke-Entry { param($value) { param($inner) $inner + $value } }; return $first }\n",
    );
    let budget = limits();
    let initial_request = request(&fixture.snapshot, &fixture.source, POWERSHELL, &budget);
    let (initial, artifact) = analyzer
        .analyze_and_capture(
            &initial_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let successor = fixture.next_generation();
    let reduced = limits_with_syntax_records(artifact.syntax_fact_count().checked_add(1).unwrap());
    let next_request = request(&successor.snapshot, &successor.source, POWERSHELL, &reduced);
    let replay = analyzer
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
    assert_eq!(replay.document(), fresh.document());
    assert_eq!(replay.report(), fresh.report());
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, entity.container))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&initial), identities(&replay));
    for entity in &replay.document().entities {
        assert_eq!(
            entity.evidence.source.as_ref().unwrap().generation(),
            successor.source.generation()
        );
    }
    for name in ["$first", "$second"] {
        let before = initial
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let after = replay
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!((before.id, before.container), (after.id, after.container));
        let owner = replay
            .document()
            .entities
            .iter()
            .find(|entity| after.container == Some(rootlight_ir::ContainerRef::Entity(entity.id)))
            .unwrap();
        assert_eq!(owner.canonical_name, "Read-Entry");
        assert_eq!(
            after.evidence.source.as_ref().unwrap().generation(),
            successor.source.generation()
        );
    }
}

#[test]
fn powershell_hashtable_entries_retain_written_keys_and_nested_ownership() {
    let source = "@{ Title = 'text'; 'quoted key' = 2; Empty = @{}; Nested = @{ Title = 'inner' }; Items = @(@{ Code = 1 }, @{ Code = 2 }) }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let properties: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 8, "{:?}", doc.skipped_regions);
    for property in properties {
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == (OccurrenceTarget::Resolved {
                            symbol: property.id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(property.canonical_name.as_str())
        );
        let owner = doc
            .entities
            .iter()
            .find(|entity| {
                property.container == Some(rootlight_ir::ContainerRef::Entity(entity.id))
            })
            .unwrap();
        assert_eq!(owner.kind, EntityKind::Namespace);
        assert!(owner.flags.contains(&EntityFlag::Synthetic));
        assert!(
            doc.relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(property.id))
        );
    }
    let codes: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "Code")
        .collect();
    assert_eq!(codes.len(), 2);
    assert_ne!(codes[0].id, codes[1].id);
    assert_ne!(codes[0].container, codes[1].container);
    let nested = doc
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Nested")
        .unwrap();
    assert!(
        doc.entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Namespace
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(nested.id)))
    );
    let changed = output(
        &source
            .replace("'text'", "'longer text'")
            .replace("= 2", "= 200"),
    );
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, (entity.canonical_name.clone(), entity.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&result), identities(&changed));
}

#[test]
fn powershell_literal_key_display_preserves_native_definitions_and_written_identity() {
    for (written, display) in [
        ("'quoted key'", "quoted key"),
        ("'can''t'", "can't"),
        ("\"a\"\"b\"", "a\"b"),
        ("\"`$key\"", "$key"),
        ("'literal $key'", "literal $key"),
        ("\"`u{96ea}\"", "雪"),
        ("\"`u{1f44d}\"", "👍"),
        ("\"a``b\"", "a`b"),
        ("@'\r\nhere key\r\n'@", "here key"),
        ("@\"\n`u{96ea}\n\"@", "雪"),
    ] {
        let source = format!("@{{ {written} = 1 }}\n");
        let result = output(&source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{written}: {:?}",
            doc.diagnostics
        );
        let properties: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Property)
            .collect();
        assert_eq!(properties.len(), 1, "{written}");
        let property = properties[0];
        assert_eq!(property.canonical_name, written);
        assert_eq!(property.display_name, display);
        assert!(!property.flags.contains(&EntityFlag::Synthetic));
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == (OccurrenceTarget::Resolved {
                            symbol: property.id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
    }
}

#[test]
fn powershell_literal_data_keeps_runtime_comparison_uncertainty_scoped() {
    let source = "@{ ModuleVersion = '1.0.0'; RootModule = 'catalog.psm1' }\n";
    let result = output(source);
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "powershell-hashtable-runtime-key-comparison-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    let span = gaps[0].source.span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some(source.trim_end())
    );
    assert_eq!(gaps[0].domain, FactDomain::Relations);
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Entities)
    );
}

#[test]
fn powershell_computed_keys_preserve_entry_ownership_without_invented_definitions() {
    let source = "@{ $key = @{ Inner = 1 }; \"$name\" = 2; (Get-Key) = 3; 'literal $name' = 4; \"\" = 5; 10 = 6 }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let dynamic: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Property && entity.flags.contains(&EntityFlag::Synthetic)
        })
        .collect();
    assert_eq!(dynamic.len(), 3);
    assert_eq!(
        dynamic
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    for entry in &dynamic {
        assert_eq!(entry.canonical_name, "<computed-key>");
        assert!(
            !doc.occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entry.id }))
        );
    }
    let gaps: Vec<_> = doc
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "powershell-computed-key-value-unavailable")
        .collect();
    assert_eq!(gaps.len(), 3);
    let written: BTreeSet<_> = gaps
        .iter()
        .map(|gap| {
            assert_eq!(gap.domain, FactDomain::Entities);
            let span = gap.source.span();
            source
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap()
        })
        .collect();
    assert_eq!(written, BTreeSet::from(["$key", "\"$name\"", "(Get-Key)"]));
    for name in ["'literal $name'", "\"\"", "10", "Inner"] {
        assert!(
            doc.entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Property
                    && entity.canonical_name == name
                    && !entity.flags.contains(&EntityFlag::Synthetic))
        );
    }
    let inner = doc
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Inner")
        .unwrap();
    let owner = doc
        .entities
        .iter()
        .find(|entity| inner.container == Some(rootlight_ir::ContainerRef::Entity(entity.id)))
        .unwrap();
    assert!(
        dynamic
            .iter()
            .any(|entry| owner.container == Some(rootlight_ir::ContainerRef::Entity(entry.id)))
    );
}
