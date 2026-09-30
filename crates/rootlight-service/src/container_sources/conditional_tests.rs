//! Closed Boolean compiler conditions select bindings without deleting source facts.
//! Authored native-container fixtures verify exact same-module declarations and
//! relationships through branch edits, durable reopen, and clean rebuild.

use super::*;
use rootlight_ir::{EntityRecord, OccurrenceRecord, OccurrenceRole};

const ARMS: [&str; 3] = ["then", "elseif", "else"];
const FIELD_MARKERS: [&str; 3] = ["Value As Long", "Value As Integer", "Value As String"];

fn source_file(document: &NormalizedIrDocument, text: &str) -> FileId {
    let hash = content_hash(text.as_bytes());
    let mut files = document
        .files
        .iter()
        .filter(|file| file.language == "vba" && file.content_hash == hash);
    let file = files.next().expect("exact authored module is retained");
    assert!(
        files.next().is_none(),
        "authored module identity must be unique"
    );
    file.id
}

fn conditional_fixture(header: &str, first: &str, second: &str) -> String {
    let mut text = format!("Attribute VB_Name = \"Client\"\r\n{header}#If {first} Then\r\n");
    for (index, arm) in ARMS.into_iter().enumerate() {
        if index == 1 {
            text.push_str(&format!("#ElseIf {second} Then\r\n"));
        } else if index == 2 {
            text.push_str("#Else\r\n");
        }
        text.push_str(&format!(
            "' {arm} type\r\nPrivate Type Record\r\n{}\r\nEnd Type\r\n' {arm} procedure\r\nPrivate Sub Visit(ByVal value As Long)\r\nCall ALWAYS()\r\nEnd Sub\r\n",
            FIELD_MARKERS[index]
        ));
    }
    text.push_str(
        "#End If\r\nPrivate Sub Always()\r\nEnd Sub\r\nPublic Sub Run()\r\nDim item As Record\r\nitem.Value = 1\r\nCall VISIT(value:=1)\r\nGoTo Complete\r\n",
    );
    text.push_str(&format!("#If {first} Then\r\n"));
    for (index, arm) in ARMS.into_iter().enumerate() {
        if index == 1 {
            text.push_str(&format!("#ElseIf {second} Then\r\n"));
        } else if index == 2 {
            text.push_str("#Else\r\n");
        }
        text.push_str(&format!("' {arm} label\r\nComplete:\r\n"));
    }
    text.push_str("#End If\r\nEnd Sub\r\n");
    text
}

fn definition<'a>(
    document: &'a NormalizedIrDocument,
    text: &str,
    marker: &str,
    name: &str,
    kind: EntityKind,
) -> (&'a OccurrenceRecord, &'a EntityRecord) {
    let start = u64::try_from(text.find(marker).unwrap() + marker.rfind(name).unwrap()).unwrap();
    let file = source_file(document, text);
    let mut sites = document.occurrences.iter().filter(|site| {
        site.file == file
            && site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == start
    });
    let site = sites
        .next()
        .unwrap_or_else(|| panic!("missing retained definition: {marker}"));
    assert!(
        sites.next().is_none(),
        "duplicate definition source: {marker}"
    );
    let OccurrenceTarget::Resolved { symbol } = site.target else {
        panic!("a retained definition must keep its own identity: {marker}")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.generation, document.generation);
    assert!(
        entity.evidence.source.is_some(),
        "declaration bytes must remain retrievable"
    );
    (site, entity)
}

fn occurrence<'a>(
    document: &'a NormalizedIrDocument,
    text: &str,
    marker: &str,
    spelling: &str,
    syntax: &str,
) -> &'a OccurrenceRecord {
    let start = u64::try_from(text.find(marker).unwrap() + marker.find(spelling).unwrap()).unwrap();
    let end = start + u64::try_from(spelling.len()).unwrap();
    let file = source_file(document, text);
    let mut sites = document.occurrences.iter().filter(|site| {
        site.file == file
            && site.syntax_kind == syntax
            && site.source.span().start_byte() == start
            && site.source.span().end_byte() == end
    });
    let site = sites
        .next()
        .unwrap_or_else(|| panic!("missing exact occurrence: {syntax} {marker}"));
    assert!(
        sites.next().is_none(),
        "duplicate exact occurrence: {syntax} {marker}"
    );
    site
}

fn assert_edge(
    document: &NormalizedIrDocument,
    site: &OccurrenceRecord,
    target: Option<SymbolId>,
    predicate: RelationPredicate,
) {
    if let Some(symbol) = target {
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol },
            "{} at {:?}",
            site.syntax_kind,
            site.source.span()
        );
    } else {
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{:?}",
            site.target
        );
    }
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.subject == RelationEndpoint::Occurrence(site.id) && edge.predicate == predicate
        })
        .collect();
    assert_eq!(edges.len(), usize::from(target.is_some()));
    for edge in edges {
        assert_eq!(edge.object, RelationEndpoint::Entity(target.unwrap()));
        assert_eq!(edge.evidence.source.as_ref(), Some(&site.source));
        assert_eq!(edge.provenance, site.provenance);
        assert_eq!(edge.generation, site.generation);
        assert_eq!(edge.repository, site.repository);
    }
}

fn assert_closed_bindings(
    service: &FirstSliceService,
    generation: GenerationId,
    text: &str,
    active: Option<usize>,
) {
    let snapshot = service.loaded_generation_snapshot(generation).unwrap();
    let document = snapshot.document();
    let mut declarations = Vec::new();
    let mut retained_sources = BTreeSet::new();
    for (index, arm) in ARMS.into_iter().enumerate() {
        let mut symbols = Vec::new();
        for (marker, name, kind) in [
            (
                format!("' {arm} type\r\nPrivate Type Record"),
                "Record",
                EntityKind::Struct,
            ),
            (FIELD_MARKERS[index].to_owned(), "Value", EntityKind::Field),
            (
                format!("' {arm} procedure\r\nPrivate Sub Visit"),
                "Visit",
                EntityKind::Function,
            ),
            (
                format!("' {arm} label\r\nComplete:"),
                "Complete",
                EntityKind::StatementLabel,
            ),
        ] {
            let (site, entity) = definition(document, text, &marker, name, kind);
            symbols.push(entity.id);
            retained_sources.insert(site.source.clone());
            retained_sources.insert(entity.evidence.source.clone().unwrap());
        }
        declarations.push(symbols);
    }
    for position in 0..4 {
        assert_eq!(
            declarations
                .iter()
                .map(|symbols| symbols[position])
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }
    let call = "Call VISIT(value:=1)";
    let sites = [
        (
            "Dim item As Record",
            "Record",
            "vba.direct_type_identifier.reference",
            0,
            RelationPredicate::UsesType,
        ),
        (
            "item.Value = 1",
            "item.Value",
            "vba.let_member.reference",
            1,
            RelationPredicate::RefersTo,
        ),
        (
            call,
            "VISIT",
            "vba.invoked_identifier.reference",
            2,
            RelationPredicate::Calls,
        ),
        (
            call,
            call,
            "vba.application.reference",
            2,
            RelationPredicate::RefersTo,
        ),
        (
            "GoTo Complete",
            "Complete",
            "vba.statement_label.reference",
            3,
            RelationPredicate::RefersTo,
        ),
    ];
    for (marker, spelling, syntax, position, predicate) in sites {
        let site = occurrence(document, text, marker, spelling, syntax);
        let target = active.map(|index| declarations[index][position]);
        assert_edge(document, site, target, predicate);
        retained_sources.insert(site.source.clone());
        for (index, symbols) in declarations.iter().enumerate() {
            let family = match predicate {
                RelationPredicate::Calls => RelationFamily::Calls,
                RelationPredicate::UsesType => RelationFamily::Types,
                _ => RelationFamily::References,
            };
            let result = service
                .symbol_relationships(
                    generation,
                    BTreeSet::from([symbols[position]]),
                    vec![family],
                    Some(RelationDirection::Inbound),
                    0,
                    32,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert!(!result.data.truncated);
            assert_eq!(
                result
                    .data
                    .groups
                    .iter()
                    .flat_map(|group| &group.items)
                    .flat_map(|edge| &edge.source_refs)
                    .any(|source| source == &site.source),
                active == Some(index)
            );
        }
    }
    if let Some(active) = active {
        let always = definition(
            document,
            text,
            "Private Sub Always",
            "Always",
            EntityKind::Function,
        )
        .1
        .id;
        for (index, arm) in ARMS.into_iter().enumerate() {
            let marker = format!(
                "' {arm} procedure\r\nPrivate Sub Visit(ByVal value As Long)\r\nCall ALWAYS()"
            );
            for (spelling, syntax, predicate) in [
                (
                    "ALWAYS",
                    "vba.invoked_identifier.reference",
                    RelationPredicate::Calls,
                ),
                (
                    "Call ALWAYS()",
                    "vba.application.reference",
                    RelationPredicate::RefersTo,
                ),
            ] {
                let site = occurrence(document, text, &marker, spelling, syntax);
                assert_edge(
                    document,
                    site,
                    (active == index).then_some(always),
                    predicate,
                );
                retained_sources.insert(site.source.clone());
            }
        }
    }
    let references: Vec<_> = retained_sources.into_iter().collect();
    for batch in references.chunks(16) {
        let read = service
            .source_read_with_options_and_budget(
                generation,
                batch.to_vec(),
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), batch.len());
        for chunk in read.data.chunks {
            assert_eq!(chunk.reference.generation(), generation);
            assert_eq!(
                chunk.reference.content_hash(),
                content_hash(text.as_bytes())
            );
            assert!(chunk.extracted_from.is_some());
            let span = chunk.reference.span();
            assert_eq!(
                chunk.bytes,
                text.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]
            );
        }
    }
}

#[test]
fn closed_boolean_branches_bind_unconditional_uses_without_losing_inactive_sources() {
    for (header, first, second, active) in [
        ("", "True", "False", 0),
        ("", "False", "True", 1),
        ("", "False", "False", 2),
        ("#Const Enabled = True\r\n", "Enabled", "False", 0),
        ("#Const Enabled = False\r\n", "Enabled", "Not Enabled", 1),
        (
            "#Const Enabled = (True Or False) And Not False\r\n",
            "Enabled Xor False",
            "False",
            0,
        ),
        (
            "#Const Enabled = True\r\nPrivate Const Enabled = False\r\n",
            "Enabled",
            "False",
            0,
        ),
    ] {
        let root = tempdir();
        let text = conditional_fixture(header, first, second);
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[("Client", text.as_bytes(), false)]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_closed_bindings(&service, receipt.generation, &text, Some(active));
    }
}

#[test]
fn closed_boolean_branch_edits_retire_bindings_after_noop_reopen_and_clean_rebuild() {
    let root = tempdir();
    let storage = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let mut previous = None;
    for (initializer, active) in [
        ("True", Some(0)),
        ("False", Some(2)),
        ("HostChoice", None),
        ("1", None),
        ("True", Some(0)),
    ] {
        let text = conditional_fixture(
            &format!("#Const Enabled = {initializer}\r\n"),
            "Enabled",
            "False",
        );
        let book = fixture::typed_book(&[("Client", text.as_bytes(), false)]);
        fs::write(root.path().join("book.xlsm"), &book).unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_ne!(previous, Some(receipt.generation));
        previous = Some(receipt.generation);
        assert_eq!(
            receipt,
            service.index_repository(root.path(), &deadline()).unwrap()
        );
        drop(service);
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            receipt,
            service.index_repository(root.path(), &deadline()).unwrap()
        );
        let path = derived_path("book.xlsm", "Client", ModuleKind::Procedural).unwrap();
        let source = read_module(&service, receipt.generation, path.as_str(), text.as_bytes());
        assert_eq!(source.generation(), receipt.generation);
        assert_closed_bindings(&service, receipt.generation, &text, active);
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(receipt.logical_snapshot, rebuilt.logical_snapshot);
        assert_closed_bindings(&service, rebuilt.generation, &text, active);
    }
}

#[test]
fn closed_boolean_nested_branches_preserve_sources_without_promoting_excluded_calls() {
    for included in [true, false] {
        let root = tempdir();
        let text = conditional_fixture("#Const Enabled = True\r\n", "Enabled", "False")
            .replace(
                "#If Enabled Then\r\n",
                if included {
                    "#If True Then\r\n#If Enabled Then\r\n"
                } else {
                    "#If False Then\r\n#If Enabled Then\r\n"
                },
            )
            .replace("#End If\r\n", "#End If\r\n#End If\r\n");
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[("Client", text.as_bytes(), false)]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_closed_bindings(&service, receipt.generation, &text, included.then_some(0));
        if !included {
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            for arm in ARMS {
                let marker = format!(
                    "' {arm} procedure\r\nPrivate Sub Visit(ByVal value As Long)\r\nCall ALWAYS()"
                );
                for (spelling, syntax, predicate) in [
                    (
                        "ALWAYS",
                        "vba.invoked_identifier.reference",
                        RelationPredicate::Calls,
                    ),
                    (
                        "Call ALWAYS()",
                        "vba.application.reference",
                        RelationPredicate::RefersTo,
                    ),
                ] {
                    let site = occurrence(snapshot.document(), &text, &marker, spelling, syntax);
                    assert_edge(snapshot.document(), site, None, predicate);
                }
            }
        }
    }
}

#[test]
fn closed_boolean_conditions_do_not_guess_hosts_runtime_values_or_invalid_modules() {
    let mut cases = vec![
        ("".to_owned(), "HostChoice", "False"),
        ("".to_owned(), "False And HostChoice", "True"),
        ("".to_owned(), "True Or HostChoice", "False"),
        (
            "#Const Enabled = False And HostChoice\r\n".to_owned(),
            "Enabled",
            "True",
        ),
        ("".to_owned(), "True", "HostChoice"),
        ("".to_owned(), "True", "True And"),
        ("".to_owned(), "True", "1 + 1"),
        (
            "Private Const RuntimeReady = True\r\n".to_owned(),
            "RuntimeReady",
            "False",
        ),
        (
            "Private RuntimeReady As Boolean\r\n".to_owned(),
            "RuntimeReady",
            "False",
        ),
        (
            "#Const Enabled = True\r\n#Const ENABLED = False\r\n".to_owned(),
            "Enabled",
            "False",
        ),
        (
            "#Const Enabled = True\r\n#If False Then\r\n#Const enabled = False\r\n#End If\r\n"
                .to_owned(),
            "True",
            "False",
        ),
        ("#Const Unrelated = 1\r\n".to_owned(), "True", "False"),
        (
            "#Const Unrelated = True And\r\n".to_owned(),
            "True",
            "False",
        ),
    ];
    for builtin in ["Win16", "Win32", "Win64", "Mac", "Vba6", "Vba7"] {
        cases.push((String::new(), builtin, "False"));
        cases.push((
            format!("#Const {} = True\r\n", builtin.to_ascii_uppercase()),
            "True",
            "False",
        ));
    }
    for (header, first, second) in cases {
        let root = tempdir();
        let text = conditional_fixture(&header, first, second);
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[("Client", text.as_bytes(), false)]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_closed_bindings(&service, receipt.generation, &text, None);
    }
}

#[test]
fn closed_boolean_exports_and_callers_gate_final_cross_module_resolution() {
    let root = tempdir();
    let storage = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let mut previous = None;
    for (export_guard, caller_guard, resolves) in [
        ("True", "True", true),
        ("False", "True", false),
        ("HostChoice", "True", false),
        ("True", "False", false),
        ("True", "HostChoice", false),
        ("True", "True", true),
    ] {
        let declarations = "Public Enum Phase\r\nReady = 1\r\nEnd Enum\r\nPublic Type Payload\r\nCount As Long\r\nEnd Type\r\nPublic Function Compute(ByVal input As Long) As Long\r\nCompute = input\r\nEnd Function\r\n";
        let exports = format!(
            "Attribute VB_Name = \"Exports\"\r\n#If {export_guard} Then\r\n{declarations}#End If\r\n"
        );
        let other =
            format!("Attribute VB_Name = \"Other\"\r\n#If False Then\r\n{declarations}#End If\r\n");
        let client = format!(
            "Attribute VB_Name = \"Client\"\r\n#If {caller_guard} Then\r\nPublic Sub Run(ByVal state As Phase, ByRef data As Payload)\r\nDim answer As Long\r\nanswer = Phase.Ready\r\nanswer = state.Ready\r\nanswer = data.Count\r\nanswer = Exports.Compute(1)\r\nEnd Sub\r\n#End If\r\n"
        );
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[
                ("Client", client.as_bytes(), false),
                ("Exports", exports.as_bytes(), false),
                ("Other", other.as_bytes(), false),
            ]),
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_ne!(previous, Some(receipt.generation));
        previous = Some(receipt.generation);
        assert_eq!(
            receipt,
            service.index_repository(root.path(), &deadline()).unwrap()
        );
        drop(service);
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let mut symbols = Vec::new();
        let mut sources = BTreeSet::new();
        for (marker, name, kind) in [
            ("Public Enum Phase", "Phase", EntityKind::Enum),
            ("Ready = 1", "Ready", EntityKind::Constant),
            ("Public Type Payload", "Payload", EntityKind::Struct),
            ("Count As Long", "Count", EntityKind::Field),
            ("Public Function Compute", "Compute", EntityKind::Function),
        ] {
            let (site, entity) = definition(document, &exports, marker, name, kind);
            let (inactive_site, inactive_entity) = definition(document, &other, marker, name, kind);
            assert_ne!(entity.id, inactive_entity.id);
            symbols.push(entity.id);
            sources.extend([
                site.source.clone(),
                inactive_site.source.clone(),
                entity.evidence.source.clone().unwrap(),
                inactive_entity.evidence.source.clone().unwrap(),
            ]);
        }
        for (marker, spelling, syntax, position, predicate) in [
            (
                "ByVal state As Phase",
                "Phase",
                "vba.direct_type_identifier.reference",
                0,
                RelationPredicate::UsesType,
            ),
            (
                "Phase.Ready",
                "Phase.Ready",
                "vba.get_member.reference",
                1,
                RelationPredicate::RefersTo,
            ),
            (
                "ByRef data As Payload",
                "Payload",
                "vba.direct_type_identifier.reference",
                2,
                RelationPredicate::UsesType,
            ),
            (
                "data.Count",
                "data.Count",
                "vba.get_member.reference",
                3,
                RelationPredicate::RefersTo,
            ),
            (
                "Exports.Compute(1)",
                "Exports.Compute",
                "vba.invoked_get_member.reference",
                4,
                RelationPredicate::Calls,
            ),
            (
                "Exports.Compute(1)",
                "Exports.Compute(1)",
                "vba.application.reference",
                4,
                RelationPredicate::RefersTo,
            ),
        ] {
            let site = occurrence(document, &client, marker, spelling, syntax);
            assert_edge(
                document,
                site,
                resolves.then_some(symbols[position]),
                predicate,
            );
            sources.insert(site.source.clone());
            if !resolves {
                assert!(
                    !document.provenance.iter().any(|proof| {
                        proof.id == site.provenance
                            && proof
                                .rule
                                .as_deref()
                                .is_some_and(|rule| rule.starts_with("classic-project-"))
                    }),
                    "excluded or unproven source must not acquire a final project proof: {export_guard} {caller_guard} {spelling}"
                );
                assert!(!document.relations.iter().any(|edge| edge.subject
                    == RelationEndpoint::Occurrence(site.id)
                    && matches!(
                        edge.predicate,
                        RelationPredicate::RefersTo
                            | RelationPredicate::Calls
                            | RelationPredicate::UsesType
                    )));
            }
        }
        // Enum members belong to the type namespace, not to a scalar enum value
        // (MS-VBAL 5.6.12); activity must not invent an instance member.
        let enum_value_member = occurrence(
            document,
            &client,
            "state.Ready",
            "state.Ready",
            "vba.get_member.reference",
        );
        assert_edge(
            document,
            enum_value_member,
            None,
            RelationPredicate::RefersTo,
        );
        sources.insert(enum_value_member.source.clone());
        let sources: Vec<_> = sources.into_iter().collect();
        for batch in sources.chunks(16) {
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    batch.to_vec(),
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(read.data.chunks.len(), batch.len());
            for chunk in read.data.chunks {
                let text = if chunk.reference.span().file() == source_file(document, &client) {
                    &client
                } else if chunk.reference.span().file() == source_file(document, &exports) {
                    &exports
                } else {
                    assert_eq!(chunk.reference.span().file(), source_file(document, &other));
                    &other
                };
                assert_eq!(chunk.reference.generation(), receipt.generation);
                assert_eq!(
                    chunk.reference.content_hash(),
                    content_hash(text.as_bytes())
                );
                assert!(chunk.extracted_from.is_some());
                let span = chunk.reference.span();
                assert_eq!(
                    chunk.bytes,
                    text.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()]
                );
            }
        }
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            receipt.logical_snapshot,
            service
                .publish_prepared(prepared, &deadline())
                .unwrap()
                .logical_snapshot
        );
    }
}
