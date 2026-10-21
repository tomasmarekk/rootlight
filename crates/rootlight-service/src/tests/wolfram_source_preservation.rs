//! Wolfram routing retains exact source beside finite native written structure.
//! Runtime evaluation and unsupported patterns remain explicit gaps; native
//! admission cannot borrow Objective-C identity or imported evaluation targets.

use super::*;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};

const PACKAGE: &[u8] =
    include_bytes!("../../../rootlight-discovery/tests/fixtures/wolfram/OriginalPackage.m");
const PACLET: &[u8] =
    include_bytes!("../../../rootlight-discovery/tests/fixtures/wolfram/OriginalPaclet.m");
const INIT: &[u8] =
    include_bytes!("../../../rootlight-discovery/tests/fixtures/wolfram/OriginalInit.m");

fn encode(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf-8" => text.as_bytes().to_vec(),
        "utf-8-bom" => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
        "utf-16le" => std::iter::once(0xfeff_u16)
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect(),
        "utf-16be" => std::iter::once(0xfeff_u16)
            .chain(text.encode_utf16())
            .flat_map(u16::to_be_bytes)
            .collect(),
        _ => unreachable!("the source fixture has four authored encodings"),
    }
}

fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn sibling(root: &Path) {
    write(
        root,
        "lib.rs",
        b"pub fn retained_wolfram_sibling() -> usize { 1 }\n",
    );
}

fn exact_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    bytes: &[u8],
) -> SourceRef {
    let cancel = Cancellation::new();
    let lease = service
        .generation_lease(receipt.generation, &cancel)
        .unwrap();
    let document = lease.generation().document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    let source = file.evidence.source.clone().unwrap();
    assert_eq!(file.repository, receipt.repository);
    assert_eq!(file.generation, receipt.generation);
    assert_eq!(file.content_hash, content_hash(bytes));
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(source.repository(), receipt.repository);
    assert_eq!(source.generation(), receipt.generation);
    assert_eq!(source.content_hash(), file.content_hash);
    assert_eq!(
        source.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancel,
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, source);
    assert_eq!(read.data.chunks[0].bytes, bytes);
    source
}

fn preserved_bounded_native(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    text: &str,
    encoding: &str,
    literal: &str,
) {
    let bytes = encode(text, encoding);
    let physical = |offset| match encoding {
        "utf-8" => offset,
        "utf-8-bom" => 3 + offset,
        "utf-16le" | "utf-16be" => 2 + text[..offset].encode_utf16().count() * 2,
        _ => unreachable!(),
    };
    let source = exact_source(service, receipt, path, &bytes);
    let cancel = Cancellation::new();
    let lease = service
        .generation_lease(receipt.generation, &cancel)
        .unwrap();
    let document = lease.generation().document();
    let file = document
        .files
        .iter()
        .find(|file| file.id == source.span().file())
        .unwrap();
    assert_eq!(file.language, "wolfram");
    assert_eq!(
        file.encoding,
        encoding.strip_suffix("-bom").unwrap_or(encoding)
    );
    let provenance = document
        .provenance
        .iter()
        .find(|record| record.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.repository, receipt.repository);
    assert_eq!(provenance.generation, receipt.generation);
    assert_eq!(provenance.language, "wolfram");
    assert_eq!(provenance.producer_kind, rootlight_ir::ProducerKind::Parser);
    assert_eq!(provenance.tier, rootlight_ir::AnalysisTier::TierD);
    assert_eq!(provenance.rule, None);
    assert_eq!(
        provenance.input_sources.as_slice(),
        std::slice::from_ref(&source)
    );
    assert_eq!(
        provenance.evidence_sources.as_slice(),
        std::slice::from_ref(&source)
    );
    let local_entities: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|reference| reference.span().file() == file.id)
        })
        .collect();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().file() == file.id
                && site.role == rootlight_ir::OccurrenceRole::Definition
        })
        .collect();
    definitions.sort_unstable_by_key(|site| site.source.span().start_byte());
    // Only this original contains a unique head whose every formal is _String.
    // Mixed constraints, empty parameter lists and multi-clause heads are gaps.
    let expected: &[(&str, EntityKind, usize, usize)] = if text.as_bytes() == PACKAGE {
        &[
            ("getVersionFromKernel", EntityKind::Function, 2458, 2478),
            ("mathBin", EntityKind::Parameter, 2479, 2486),
        ]
    } else {
        &[]
    };
    assert_eq!(
        definitions.len(),
        expected.len(),
        "exact written definitions"
    );
    assert_eq!(
        local_entities.len(),
        expected.len(),
        "no invented declarations"
    );
    let mut symbols = Vec::new();
    for (site, &(name, kind, start, end)) in definitions.iter().zip(expected) {
        assert_eq!(&text[start..end], name);
        assert_eq!(site.repository, receipt.repository);
        assert_eq!(site.generation, receipt.generation);
        assert_eq!(site.source.content_hash(), source.content_hash());
        assert_eq!(site.source.span().file(), file.id);
        assert_eq!(
            site.source.span().start_byte(),
            u64::try_from(physical(start)).unwrap()
        );
        assert_eq!(
            site.source.span().end_byte(),
            u64::try_from(physical(end)).unwrap()
        );
        assert_eq!(site.provenance, file.provenance);
        let rootlight_ir::OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("a written definition resolves to its own native declaration");
        };
        let entity = local_entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.repository, receipt.repository);
        assert_eq!(entity.generation, receipt.generation);
        assert_eq!(entity.language, "wolfram");
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.provenance, file.provenance);
        let evidence = entity.evidence.source.as_ref().unwrap();
        assert_eq!(evidence.repository(), source.repository());
        assert_eq!(evidence.generation(), source.generation());
        assert_eq!(evidence.content_hash(), source.content_hash());
        assert_eq!(evidence.span().file(), file.id);
        assert!(evidence.span().start_byte() <= site.source.span().start_byte());
        assert!(evidence.span().end_byte() >= site.source.span().end_byte());
        assert!(evidence.span().end_byte() <= source.span().end_byte());
        symbols.push(symbol);
    }
    if let [owner, formal] = symbols.as_slice() {
        let parameter = local_entities
            .iter()
            .find(|entity| entity.id == *formal)
            .unwrap();
        assert_eq!(
            parameter.container,
            Some(rootlight_ir::ContainerRef::Entity(*owner))
        );
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == rootlight_ir::RelationPredicate::Contains
                    && edge.subject == rootlight_ir::RelationEndpoint::Entity(*owner)
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(*formal)
            })
            .collect();
        assert_eq!(edges.len(), 1, "exact written head owns its own formal");
        assert_eq!(edges[0].repository, receipt.repository);
        assert_eq!(edges[0].generation, receipt.generation);
        assert_eq!(edges[0].provenance, file.provenance);
        let evidence = edges[0].evidence.source.as_ref().unwrap();
        assert_eq!(evidence.content_hash(), source.content_hash());
        assert_eq!(evidence.span().file(), file.id);
        assert!(evidence.span().start_byte() <= u64::try_from(physical(2479)).unwrap());
        assert!(evidence.span().end_byte() >= u64::try_from(physical(2486)).unwrap());
    }
    for site in document
        .occurrences
        .iter()
        .filter(|site| site.source.span().file() == file.id)
    {
        assert_eq!(site.repository, receipt.repository);
        assert_eq!(site.generation, receipt.generation);
        assert_eq!(site.provenance, file.provenance);
        assert_eq!(site.source.content_hash(), source.content_hash());
        assert!(site.source.span().end_byte() <= source.span().end_byte());
        assert!(
            !matches!(site.target, rootlight_ir::OccurrenceTarget::Resolved { .. })
                || site.role == rootlight_ir::OccurrenceRole::Definition,
            "these originals have no certified direct formal-argument Read"
        );
    }
    assert!(
        !document.relations.iter().any(|edge| {
            edge.predicate == rootlight_ir::RelationPredicate::Calls
                && edge
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|reference| reference.span().file() == file.id)
        }),
        "source calls have no imported or evaluation-target authority"
    );
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        let gaps: Vec<_> = document.skipped_regions.iter().filter(|gap| {
            gap.source == source
                && gap.domain == domain
                && gap.reason == SkippedRegionReason::UnsupportedConstruct
                && gap.detail == "wolfram-written-delayed-functions-string-pattern-formals-and-finite-module-reads-only-runtime-evaluation-dynamic-scopes-and-import-targets-unavailable"
        }).collect();
        assert_eq!(gaps.len(), 1, "one exact whole-source native limitation");
        assert_eq!(gaps[0].repository, receipt.repository);
        assert_eq!(gaps[0].generation, receipt.generation);
        assert_eq!(gaps[0].provenance, file.provenance);
        assert_eq!(gaps[0].evidence.source.as_ref(), Some(&source));
        let records: Vec<_> = document
            .coverage_records
            .iter()
            .filter(|coverage| {
                coverage.scope == CoverageScope::File(file.id) && coverage.domain == domain
            })
            .collect();
        assert_eq!(records.len(), 1);
        let record = records[0];
        assert_eq!(record.repository, receipt.repository);
        assert_eq!(record.generation, receipt.generation);
        assert_eq!(record.provenance, file.provenance);
        assert_eq!(record.evidence.source.as_ref(), Some(&source));
        assert_eq!(record.tier, rootlight_ir::AnalysisTier::TierD);
        assert_eq!(record.status, CoverageStatus::Bounded);
        let skipped = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.source.span().file() == file.id && gap.domain == domain)
            .count();
        assert_eq!(record.skipped, u64::try_from(skipped).unwrap());
        assert!(record.skipped > 0);
        assert_eq!(
            record.discovered,
            record.indexed.checked_add(record.skipped).unwrap()
        );
    }
    assert!(!document.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .source
            .as_ref()
            .is_some_and(|reference| reference.span().file() == file.id)
            && matches!(
                diagnostic.code.as_str(),
                "unsupported-language" | "unsupported-encoding"
            )
    }));
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &cancel)
        .unwrap();
    assert_eq!(coverage.language, "wolfram");
    assert_eq!(coverage.tier, rootlight_ir::AnalysisTier::TierD);
    assert_eq!(coverage.status, CoverageStatus::Bounded);
    assert_eq!(coverage.reason, None);
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.language == "rust"
                && entity.kind == EntityKind::Function
                && entity.canonical_name == "retained_wolfram_sibling")
    );
    assert_eq!(text.match_indices(literal).count(), 1);
    let decoded_start = text.find(literal).unwrap();
    for (query, expected) in [(literal, 1), ("wolfram_source_absent_control_947", 0)] {
        let found = service
            .find_literal(
                receipt.generation,
                file.id,
                query,
                None,
                SourceFindBudget::new(),
                &cancel,
            )
            .unwrap();
        assert_eq!(found.matches.len(), expected);
        assert_eq!(found.stop, SourceFindStop::EndOfSource);
        assert!(found.continuation.is_none());
        if expected == 1 {
            assert_eq!(found.matches[0].repository(), source.repository());
            assert_eq!(found.matches[0].generation(), source.generation());
            assert_eq!(found.matches[0].content_hash(), source.content_hash());
            assert_eq!(
                found.matches[0].span().start_byte(),
                u64::try_from(physical(decoded_start)).unwrap()
            );
            assert_eq!(
                found.matches[0].span().end_byte(),
                u64::try_from(physical(decoded_start + literal.len())).unwrap()
            );
            let selected = SourceRef::new(
                source.repository(),
                source.generation(),
                found.matches[0].span(),
                source.content_hash(),
                None,
            );
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![selected.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &cancel,
                )
                .unwrap();
            assert_eq!(read.data.chunks.len(), 1);
            assert_eq!(read.data.chunks[0].reference, selected);
            assert_eq!(
                read.data.chunks[0].bytes.as_slice(),
                &bytes[physical(decoded_start)..physical(decoded_start + literal.len())]
            );
        }
    }
}

#[test]
fn wolfram_full_original_m_sources_keep_context_exact_bytes_and_bounded_native_facts() {
    assert_eq!((PACKAGE.len(), PACLET.len(), INIT.len()), (14_040, 339, 96));
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    for (path, source) in [
        ("package.m", PACKAGE),
        ("paclet.m", PACLET),
        ("init.m", INIT),
    ] {
        write(root.path(), path, source);
    }
    sibling(root.path());
    let mut service = FirstSliceService::new(2).unwrap();
    let cancel = Cancellation::new();
    let receipt = service.index_repository(root.path(), &cancel).unwrap();
    for (path, source, literal) in [
        ("package.m", PACKAGE, "BeginPackage"),
        ("paclet.m", PACLET, "Paclet["),
        ("init.m", INIT, "Get["),
    ] {
        preserved_bounded_native(
            &service,
            &receipt,
            path,
            std::str::from_utf8(source).unwrap(),
            "utf-8",
            literal,
        );
    }
    assert_eq!(
        service.index_repository(root.path(), &cancel).unwrap(),
        receipt
    );
}

#[test]
fn wolfram_m_headers_need_no_peer_but_the_original_get_init_does() {
    for (source, literal) in [(PACKAGE, "BeginPackage"), (PACLET, "Paclet[")] {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write(root.path(), "arbitrary.m", source);
        sibling(root.path());
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(root.path(), &Cancellation::new())
            .unwrap();
        preserved_bounded_native(
            &service,
            &receipt,
            "arbitrary.m",
            std::str::from_utf8(source).unwrap(),
            "utf-8",
            literal,
        );
    }
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    write(root.path(), "init.m", INIT);
    sibling(root.path());
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(root.path(), &Cancellation::new())
        .unwrap();
    exact_source(&service, &receipt, "init.m", INIT);
    let lease = service
        .generation_lease(receipt.generation, &Cancellation::new())
        .unwrap();
    assert_ne!(
        lease
            .generation()
            .document()
            .files
            .iter()
            .find(|file| file.path == "init.m")
            .unwrap()
            .language,
        "wolfram"
    );
}

#[test]
fn wolfram_suffix_sources_keep_names_and_foreign_m_does_not_borrow_wolfram() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let written = "(* λ 😀 *)\nwrittenValue[arg_] := arg + 1;\n";
    let script = "#!/usr/bin/env wolframscript\nwrittenValue[arg_] := arg + 1;\n";
    let foreign = b"@interface Ordinary\n- (void)run;\n@end\n";
    write(root.path(), "source.wl", written.as_bytes());
    write(root.path(), "script.WLS", script.as_bytes());
    write(root.path(), "foreign.m", foreign);
    sibling(root.path());
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(root.path(), &Cancellation::new())
        .unwrap();
    preserved_bounded_native(
        &service,
        &receipt,
        "source.wl",
        written,
        "utf-8",
        "writtenValue",
    );
    preserved_bounded_native(
        &service,
        &receipt,
        "script.WLS",
        script,
        "utf-8",
        "writtenValue",
    );
    exact_source(&service, &receipt, "foreign.m", foreign);
    let lease = service
        .generation_lease(receipt.generation, &Cancellation::new())
        .unwrap();
    assert_eq!(
        lease
            .generation()
            .document()
            .files
            .iter()
            .find(|file| file.path == "foreign.m")
            .unwrap()
            .language,
        "objective-c"
    );
}

#[test]
fn wolfram_complete_originals_preserve_utf8_bom_and_both_utf16_orders() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let unicode = "(* λ 😀 *)\r\nwrittenValue[arg_] := arg;\r\n";
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        for (stem, source) in [("package", PACKAGE), ("paclet", PACLET), ("init", INIT)] {
            let text = std::str::from_utf8(source).unwrap();
            write(
                root.path(),
                &format!("{stem}-{encoding}.m"),
                &encode(text, encoding),
            );
        }
        write(
            root.path(),
            &format!("unicode-{encoding}.wl"),
            &encode(unicode, encoding),
        );
    }
    sibling(root.path());
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(root.path(), &Cancellation::new())
        .unwrap();
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        preserved_bounded_native(
            &service,
            &receipt,
            &format!("unicode-{encoding}.wl"),
            unicode,
            encoding,
            "writtenValue",
        );
        for (stem, source, literal) in [
            ("package", PACKAGE, "BeginPackage"),
            ("paclet", PACLET, "Paclet["),
            ("init", INIT, "Get["),
        ] {
            preserved_bounded_native(
                &service,
                &receipt,
                &format!("{stem}-{encoding}.m"),
                std::str::from_utf8(source).unwrap(),
                encoding,
                literal,
            );
        }
    }
}
