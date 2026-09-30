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

const COMPILATION_WORK_GROUPS: usize = 96;
const COMPILATION_WORK_STATEMENTS: usize = 768;

fn compilation_work_deadline() -> Cancellation {
    // This larger durable fixture tests the default work ceiling, not indexing
    // latency. Debug builds and slow runners need a separate wall-clock guard.
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(180))
            .expect("test deadline is representable"),
    )
}

fn compilation_work_module(name: &str, tail: &str) -> String {
    use std::fmt::Write as _;

    let mut text = format!("Attribute VB_Name = \"{name}\"\r\n");
    for index in 0..COMPILATION_WORK_GROUPS {
        write!(
            text,
            "#If True Then\r\nPrivate Const Included{index} As Long = {index}\r\n#Else\r\nPrivate Const Excluded{index} As Long = {index}\r\n#End If\r\n"
        )
        .unwrap();
    }
    text.push_str("Public Sub Run()\r\nDim value As String\r\nDim result As Long\r\n");
    for _ in 0..COMPILATION_WORK_STATEMENTS {
        text.push_str("value = VBA.Chr(65) & VBA.LCase(\"A\")\r\n");
    }
    text.push_str(tail);
    text.push_str("End Sub\r\n");
    text
}

fn assert_default_work_compilation_bindings(
    service: &FirstSliceService,
    generation: GenerationId,
    modules: &[(&str, &str)],
) {
    let snapshot = service.loaded_generation_snapshot(generation).unwrap();
    let document = snapshot.document();
    assert!(
        !document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "resolution-work-bounded"),
        "unrelated conditional inventory must not consume the default resolution budget"
    );
    let inventories: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::VBA_COMPILATION_NAMESPACE)
        .collect();
    assert_eq!(inventories.len(), modules.len());
    for &(name, text) in modules {
        let file = source_file(document, text);
        let envelope = inventories
            .iter()
            .find(|envelope| {
                envelope
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file)
            })
            .unwrap();
        let inventory = rootlight_ir::decode_vba_compilation_envelope(envelope).unwrap();
        let source = envelope.evidence.source.as_ref().unwrap();
        assert!(inventory.complete);
        assert_eq!(source.span(), inventory.scope);
        assert_eq!(source.span().start_byte(), 0);
        assert_eq!(source.span().end_byte(), u64::try_from(text.len()).unwrap());
        assert_eq!(source.repository(), document.repository);
        assert_eq!(source.generation(), generation);
        assert_eq!(source.content_hash(), content_hash(text.as_bytes()));
        assert_eq!(envelope.evidence.derivation, vec![FactRef::File(file)]);
        let parser = document
            .provenance
            .iter()
            .find(|parser| parser.id == envelope.provenance)
            .unwrap();
        assert_eq!(parser.producer_kind, rootlight_ir::ProducerKind::Parser);
        assert!(parser.input_sources.contains(source));
        if name == "Exports" {
            assert_eq!(inventory.branches.len(), 3);
            assert_eq!(
                inventory
                    .branches
                    .iter()
                    .map(|branch| branch.activity)
                    .collect::<Vec<_>>(),
                vec![
                    rootlight_ir::VbaCompilationActivity::Active,
                    rootlight_ir::VbaCompilationActivity::Inactive,
                    rootlight_ir::VbaCompilationActivity::Unproven,
                ]
            );
        } else {
            assert_eq!(inventory.branches.len(), COMPILATION_WORK_GROUPS * 2);
            for alternatives in inventory.branches.chunks_exact(2) {
                assert_eq!(alternatives[0].group, alternatives[1].group);
                assert_eq!(
                    alternatives[0].activity,
                    rootlight_ir::VbaCompilationActivity::Active
                );
                assert_eq!(
                    alternatives[1].activity,
                    rootlight_ir::VbaCompilationActivity::Inactive
                );
            }
        }
        let read = service
            .source_read_with_options_and_budget(
                generation,
                vec![source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &compilation_work_deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, *source);
        assert_eq!(read.data.chunks[0].bytes, text.as_bytes());
        assert!(read.data.chunks[0].extracted_from.is_some());
    }

    let client = modules
        .iter()
        .find(|(name, _)| *name == "Client")
        .unwrap()
        .1;
    let exports = modules
        .iter()
        .find(|(name, _)| *name == "Exports")
        .unwrap()
        .1;
    let (_, compute) = definition(
        document,
        exports,
        "Public Function Compute",
        "Compute",
        EntityKind::Function,
    );
    for (spelling, syntax, predicate) in [
        (
            "Exports.Compute",
            "vba.invoked_get_member.reference",
            RelationPredicate::Calls,
        ),
        (
            "Exports.Compute(1)",
            "vba.application.reference",
            RelationPredicate::RefersTo,
        ),
    ] {
        let site = occurrence(
            document,
            client,
            "result = Exports.Compute(1)",
            spelling,
            syntax,
        );
        assert_edge(document, site, Some(compute.id), predicate);
    }
    for (spelling, syntax, predicate, expected_name, expected_rule) in [
        (
            "VBA.Chr(66)",
            "vba.application.reference",
            RelationPredicate::Calls,
            "VBA.Chr",
            "vba-standard-call-v1.indexed_snapshot",
        ),
        (
            "VBA.vbCrLf",
            "vba.get_member.reference",
            RelationPredicate::RefersTo,
            "VBA.Constants.vbCrLf",
            "vba-standard-constant-v1.indexed_snapshot",
        ),
    ] {
        let site = occurrence(document, client, spelling, spelling, syntax);
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("standard library binding must survive default work pressure: {spelling}")
        };
        let target = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(target.kind, EntityKind::ExternalSymbol);
        assert_eq!(target.qualified_name, expected_name);
        assert!(target.flags.contains(&EntityFlag::External));
        assert!(target.flags.contains(&EntityFlag::Synthetic));
        assert!(target.evidence.source.is_none());
        assert_edge(document, site, Some(symbol), predicate);
        assert_eq!(
            document
                .provenance
                .iter()
                .find(|proof| proof.id == site.provenance)
                .unwrap()
                .rule
                .as_deref(),
            Some(expected_rule)
        );
    }
    for name in ["Excluded", "Unknown"] {
        definition(
            document,
            exports,
            &format!("Public Function {name}"),
            name,
            EntityKind::Function,
        );
        for (spelling, syntax, predicate) in [
            (
                format!("Exports.{name}"),
                "vba.invoked_get_member.reference",
                RelationPredicate::Calls,
            ),
            (
                format!("Exports.{name}(1)"),
                "vba.application.reference",
                RelationPredicate::RefersTo,
            ),
        ] {
            let site = occurrence(document, client, &spelling, &spelling, syntax);
            assert_edge(document, site, None, predicate);
        }
    }
}

fn compilation_equivalence_diagnostics(
    service: &FirstSliceService,
    cold: GenerationId,
    rebuilt: GenerationId,
) -> serde_json::Value {
    let summarize = |generation| {
        let snapshot = service.loaded_generation_snapshot(generation).unwrap();
        let document = snapshot.document();
        let digests = rootlight_ir::canonical_generation_neutral_digests(
            document,
            service.analysis_limits.ir(),
            &service.extensions,
            || true,
        )
        .unwrap();
        let mut rules = BTreeMap::<String, usize>::new();
        let mut targets = BTreeMap::<String, usize>::new();
        let entities: BTreeMap<_, _> = document
            .entities
            .iter()
            .map(|entity| (entity.id, entity.qualified_name.as_str()))
            .collect();
        let proofs: BTreeMap<_, _> = document
            .provenance
            .iter()
            .map(|proof| {
                let rule = proof.rule.as_deref().unwrap_or("parser");
                *rules.entry(rule.to_owned()).or_default() += 1;
                (proof.id, rule)
            })
            .collect();
        for site in &document.occurrences {
            let target = match &site.target {
                OccurrenceTarget::Resolved { symbol } => entities[symbol].to_owned(),
                OccurrenceTarget::Candidates { symbols, .. } => {
                    format!("candidates:{}", symbols.len())
                }
                OccurrenceTarget::Unresolved { .. } => "unresolved".to_owned(),
            };
            let key = format!(
                "{}:{}:{}:{}:{:?}:{}:{}",
                site.file,
                site.source.span().start_byte(),
                site.source.span().end_byte(),
                site.syntax_kind,
                site.role,
                target,
                proofs[&site.provenance]
            );
            *targets.entry(key).or_default() += 1;
        }
        let sources: BTreeMap<_, _> = document
            .files
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    (file.content_hash, file.byte_length, file.language.as_str()),
                )
            })
            .collect();
        let summary = serde_json::json!({
            "neutral": {
                "complete": digests.complete(),
                "coverage": digests.coverage(),
                "provenance": digests.provenance(),
                "stable_identities": digests.stable_identities(),
            },
            "counts": {
                "files": document.files.len(), "entities": document.entities.len(),
                "occurrences": document.occurrences.len(), "relations": document.relations.len(),
                "provenance": document.provenance.len(), "coverage": document.coverage_records.len(),
                "skipped": document.skipped_regions.len(), "diagnostics": document.diagnostics.len(),
                "extensions": document.extensions.len(),
            },
            "source_fingerprint": content_hash(&serde_json::to_vec(&sources).unwrap()),
            "provenance_rule_counts": rules.iter().take(16).collect::<BTreeMap<_, _>>(),
            "provenance_rules_fingerprint": content_hash(&serde_json::to_vec(&rules).unwrap()),
            "occurrence_semantics_fingerprint": content_hash(&serde_json::to_vec(&targets).unwrap()),
            "diagnostics": document.diagnostics.iter().take(8).map(|diagnostic| {
                serde_json::json!({"code": diagnostic.code, "message": diagnostic.message.chars().take(160).collect::<String>()})
            }).collect::<Vec<_>>(),
        });
        (summary, targets)
    };
    let (cold_summary, cold_targets) = summarize(cold);
    let (rebuilt_summary, rebuilt_targets) = summarize(rebuilt);
    let keys: BTreeSet<_> = cold_targets.keys().chain(rebuilt_targets.keys()).collect();
    let changed: Vec<_> = keys
        .into_iter()
        .filter(|key| cold_targets.get(*key) != rebuilt_targets.get(*key))
        .collect();
    serde_json::json!({
        "cold": cold_summary, "rebuilt": rebuilt_summary,
        "changed_occurrence_keys": changed.len(),
        "occurrence_difference_samples": changed.iter().take(8).map(|key| {
            serde_json::json!({"key": key, "cold_count": cold_targets.get(*key), "rebuilt_count": rebuilt_targets.get(*key)})
        }).collect::<Vec<_>>(),
    })
}

#[test]
fn conditional_inventory_preserves_bindings_under_default_work_pressure() {
    let root = tempdir();
    let storage = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let client = compilation_work_module(
        "Client",
        "result = Exports.Compute(1)\r\nresult = Exports.Excluded(1)\r\nresult = Exports.Unknown(1)\r\nvalue = VBA.Chr(66)\r\nvalue = VBA.vbCrLf\r\n",
    );
    let extra_a = compilation_work_module("ExtraA", "");
    let extra_b = compilation_work_module("ExtraB", "");
    let exports = "Attribute VB_Name = \"Exports\"\r\n#If True Then\r\nPublic Function Compute(ByVal input As Long) As Long\r\nCompute = input\r\nEnd Function\r\n#End If\r\n#If False Then\r\nPublic Function Excluded(ByVal input As Long) As Long\r\nExcluded = input\r\nEnd Function\r\n#End If\r\n#If HostChoice Then\r\nPublic Function Unknown(ByVal input As Long) As Long\r\nUnknown = input\r\nEnd Function\r\n#End If\r\n";
    // Each native container fits the authored single-FAT fixture. All modules
    // still share one default resolution budget, exposing per-use branch scans.
    for (path, modules) in [
        (
            "client.xlsm",
            vec![
                ("Client", client.as_bytes(), false),
                ("Exports", exports.as_bytes(), false),
            ],
        ),
        ("extra-a.xlsm", vec![("ExtraA", extra_a.as_bytes(), false)]),
        ("extra-b.xlsm", vec![("ExtraB", extra_b.as_bytes(), false)]),
    ] {
        fs::write(root.path().join(path), fixture::typed_book(&modules)).unwrap();
    }
    let modules = [
        ("Client", client.as_str()),
        ("Exports", exports),
        ("ExtraA", extra_a.as_str()),
        ("ExtraB", extra_b.as_str()),
    ];
    let mut service =
        FirstSliceService::new_durable(4, paths.state_dir(), &compilation_work_deadline()).unwrap();
    let receipt = service
        .index_repository(root.path(), &compilation_work_deadline())
        .unwrap();
    assert_default_work_compilation_bindings(&service, receipt.generation, &modules);
    assert_eq!(
        receipt,
        service
            .index_repository(root.path(), &compilation_work_deadline())
            .unwrap()
    );
    drop(service);
    let mut service =
        FirstSliceService::new_durable(4, paths.state_dir(), &compilation_work_deadline()).unwrap();
    assert_eq!(
        receipt,
        service
            .index_repository(root.path(), &compilation_work_deadline())
            .unwrap()
    );
    assert_default_work_compilation_bindings(&service, receipt.generation, &modules);
    let prepared = service
        .prepare_repository_with_options(
            root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &compilation_work_deadline(),
        )
        .unwrap();
    let rebuilt = service
        .publish_prepared(prepared, &compilation_work_deadline())
        .unwrap();
    assert_default_work_compilation_bindings(&service, rebuilt.generation, &modules);
    assert!(receipt.logical_snapshot.is_some());
    assert_eq!(
        receipt.logical_snapshot,
        rebuilt.logical_snapshot,
        "{}",
        compilation_equivalence_diagnostics(&service, receipt.generation, rebuilt.generation)
    );
}
