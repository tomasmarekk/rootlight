//! Dynamic shell constructs retain unresolved variable targets.

use super::*;

fn run(raw: &[u8]) -> AnalysisOutput {
    let parser = Arc::new(provider());
    let fixture = Fixture::new(BASH_CASE, raw);
    let output = analyze(
        &analyzer(&parser, BASH_CASE),
        &request(&fixture.snapshot, &fixture.source, BASH_CASE, &limits()),
        &ExtensionSupport::default(),
    );
    validate_ir_document(
        output.document(),
        limits().ir(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    output
}

#[test]
fn dynamic_scope_source_eval_reassignment_and_loop_writes_keep_targets_unproved() {
    for raw in [
        "WRITTEN=outer\nfunction use_it(){ printf '%s' \"$WRITTEN\"; }\nuse_it\n",
        "WRITTEN=outer\nsource other.sh\nprintf '%s' \"$WRITTEN\"\n",
        "WRITTEN=outer\neval other\nprintf '%s' \"$WRITTEN\"\n",
        "WRITTEN=outer\nWRITTEN=later\nprintf '%s' \"$WRITTEN\"\n",
        "WRITTEN=outer\nfor WRITTEN in one two; do printf '%s' \"$WRITTEN\"; done\n",
        "WRITTEN=outer\n\"$dynamic\"\nprintf '%s' \"$WRITTEN\"\n",
        "WRITTEN=outer\nchange(){ WRITTEN=other; }\nchange\nprintf '%s' \"$WRITTEN\"\n",
    ] {
        let output = run(raw.as_bytes());
        let start = raw.rfind("$WRITTEN").unwrap() as u64 + 1;
        assert!(
            !output.document().occurrences.iter().any(|site| {
                matches!(site.role, OccurrenceRole::Reference | OccurrenceRole::Read)
                    && site.source.span().start_byte() == start
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }),
            "{raw}"
        );
    }
}
