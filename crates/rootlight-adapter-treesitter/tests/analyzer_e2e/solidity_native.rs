//! Source-backed Solidity declarations through the production native analyzer.
//! Runtime dispatch and inline assembly must remain explicit, bounded gaps.

use super::*;

pub(super) const SOLIDITY: LanguageCase = LanguageCase {
    name: "solidity",
    path: "src/vault.sol",
    frontend: "tree-sitter-solidity-1.2.13",
    source: "contract Vault { function count() public pure returns (uint) { return 1; } }",
    generated: false,
    body_before: "return 1;",
    body_after: "return 2;",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(SOLIDITY, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, SOLIDITY),
        &request(&fixture.snapshot, &fixture.source, SOLIDITY, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn solidity_declarations_have_exact_kinds_and_written_definitions() {
    let source = r#"
pragma solidity ^0.8.20;
import {Peer} from "./peer.sol";
type Amount is uint256;
interface Readable { function read() external returns (uint); }
library Arithmetic { function double(uint x) internal pure returns (uint) { return x * 2; } }
contract Vault {
    struct Entry { uint amount; }
    enum Status { Open, Closed }
    uint public balance;
    uint constant LIMIT = 10;
    event Changed(uint value);
    error Rejected(uint code);
    modifier allowed(uint minimum) { require(balance >= minimum); _; }
    constructor(uint initial) { balance = initial; }
    receive() external payable {}
    fallback() external payable {}
    function deposit(uint delta) public allowed(0) {
        uint next = balance + delta;
        balance = next;
        emit Changed(next);
    }
}
"#;
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("Amount", EntityKind::TypeAlias),
        ("Readable", EntityKind::Interface),
        ("Arithmetic", EntityKind::Class),
        ("Vault", EntityKind::Class),
        ("Entry", EntityKind::Struct),
        ("Status", EntityKind::Enum),
        ("Open", EntityKind::Constant),
        ("Closed", EntityKind::Constant),
        ("balance", EntityKind::Field),
        ("LIMIT", EntityKind::Constant),
        ("Changed", EntityKind::Event),
        ("Rejected", EntityKind::ErrorDeclaration),
        ("allowed", EntityKind::Modifier),
        ("constructor", EntityKind::Constructor),
        ("receive", EntityKind::Method),
        ("fallback", EntityKind::Method),
        ("deposit", EntityKind::Method),
        ("next", EntityKind::Variable),
        ("amount", EntityKind::Field),
        ("minimum", EntityKind::Parameter),
    ] {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "{name}: {:?}; gaps: {:?}",
            document.entities,
            document.skipped_regions
        );
        let entity = entities[0];
        assert_eq!(entity.kind, kind, "{name}");
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let definition = definitions[0];
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
        assert_eq!(
            definition.syntactic_text_hash,
            content_hash(name.as_bytes())
        );
        assert_eq!(
            definition.source.generation(),
            entity.evidence.source.as_ref().unwrap().generation()
        );
    }
}

#[test]
fn solidity_overloads_keep_distinct_parameter_owners_and_stable_body_identity() {
    let source = r#"contract Vault {
event Changed(uint value);
event Changed(address value);
error Rejected(uint code);
error Rejected(address code);
function update(uint value) public { emit Changed(value); }
function update(address value) public { emit Changed(value); }
modifier allowed(uint minimum) { require(minimum > 0); _; }
}"#;
    let initial = output(source);
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
    assert_eq!(
        identities(&initial).len(),
        initial.document().entities.len()
    );
    for (name, count) in [
        ("Changed", 2),
        ("Rejected", 2),
        ("update", 2),
        ("value", 4),
        ("code", 2),
    ] {
        let entities: Vec<_> = initial
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(
            entities.len(),
            count,
            "{name}: {:?}",
            initial.document().skipped_regions
        );
        assert_eq!(
            entities
                .iter()
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
                .len(),
            count
        );
    }
    for changed in [
        format!("// moved source\n{source}"),
        source.replace(
            "emit Changed(value);",
            "emit Changed(value); emit Changed(value);",
        ),
        source.replace("minimum > 0", "minimum > 1"),
    ] {
        assert_eq!(
            identities(&initial),
            identities(&output(&changed)),
            "{changed}"
        );
    }
}

#[test]
fn solidity_qualified_and_computed_calls_never_invent_resolved_targets() {
    let source = "contract Vault { function apply(uint value) public {} function run(Peer peer) public { apply(1); peer.apply(2); this.apply(3); factory()(4); } }";
    let result = output(source);
    let calls: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 5);
    for call in calls {
        assert!(
            matches!(call.target, OccurrenceTarget::Unresolved { .. }),
            "{call:?}"
        );
    }
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "solidity-import-inheritance-dispatch-resolution-unavailable")
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn solidity_comments_strings_and_assembly_do_not_fabricate_declarations() {
    let source = r#"contract Vault {
// function imagined() {} event Phantom(uint value);
string constant NOTE = "function invented() {}";
function run() public { assembly { function yulOnly(value) -> answer { answer := value } } }
}"#;
    let result = output(source);
    for name in ["imagined", "Phantom", "invented", "yulOnly", "answer"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}"
        );
    }
    let gap = result
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "solidity-inline-assembly-analysis-unavailable")
        .unwrap();
    let span = gap.evidence.source.as_ref().unwrap().span();
    assert!(
        source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()]
            .starts_with("assembly")
    );
}

#[test]
fn solidity_disjoint_blocks_retain_every_local_binding() {
    let source = "contract Vault { function run() public { { uint value = 1; } { uint value = 2; } for (uint index = 0; index < 1; index++) {} for (uint index = 0; index < 2; index++) {} } }";
    let original = output(source);
    let bindings: Vec<_> = original
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "value")
        .collect();
    assert_eq!(
        bindings.len(),
        2,
        "{:?}",
        original.document().skipped_regions
    );
    assert_ne!(bindings[0].id, bindings[1].id);
    assert_eq!(
        original
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "index")
            .count(),
        2
    );
    let ids = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&original), ids(&output(&source.replace("= 1", "= 3"))));
    assert_eq!(ids(&original), ids(&output(&format!("// moved\n{source}"))));
}

#[test]
fn solidity_recovery_preserves_healthy_declarations_and_reports_the_gap() {
    let source = "contract Vault { function healthy() public {} function broken( }";
    let result = output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "healthy")
    );
    let gap = result
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.reason == SkippedRegionReason::ParseError)
        .expect("malformed syntax has a parse-specific source gap");
    let span = gap.evidence.source.as_ref().unwrap().span();
    let start = usize::try_from(span.start_byte()).unwrap();
    let end = usize::try_from(span.end_byte()).unwrap();
    assert!(start <= end && end <= source.len());
    assert!(source[start..end].contains("broken"));
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}
