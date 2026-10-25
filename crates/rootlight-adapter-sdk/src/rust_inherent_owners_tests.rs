//! Source-bound proof regressions use explicit native-contract capture fixtures.
//! Native analyzer and route tests independently exercise actual parser output.

use super::*;
use crate::SyntaxKindLabel;
use rootlight_ids::{FileId, GenerationId, RepositoryId};

struct Fixture<'a> {
    text: &'a str,
    source: SourceRef,
    facts: Vec<SyntaxFact>,
}

impl<'a> Fixture<'a> {
    fn new(text: &'a str) -> Self {
        let source = SourceRef::new(
            RepositoryId::from_bytes([3; 16]),
            GenerationId::from_bytes([5; 20]),
            SourceSpan::new(FileId::from_bytes([7; 20]), 0, text.len() as u64).unwrap(),
            content_hash(text.as_bytes()),
            None,
        );
        let mut result = Self {
            text,
            source,
            facts: Vec::new(),
        };
        result.add(None, SyntaxFactKind::Root, "rust.file.root", 0, text.len());
        result.add(
            Some(1),
            SyntaxFactKind::Signature,
            "rust.proof_context.signature",
            0,
            text.len(),
        );
        result
    }
    fn add(
        &mut self,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        label: &str,
        start: usize,
        end: usize,
    ) -> u64 {
        let id = self.facts.len() as u64 + 1;
        let depth = parent.map_or(0, |id| {
            self.facts
                .iter()
                .find(|fact| fact.local_id() == id)
                .unwrap()
                .depth()
                + 1
        });
        self.facts.push(SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(self.source.span().file(), start as u64, end as u64).unwrap(),
            depth,
            SyntaxKindLabel::new(label).unwrap(),
        ));
        id
    }
    fn item(&mut self, parent: u64, anchor: &str, name: &str, label: &str) -> u64 {
        let start = self.text.find(anchor).unwrap();
        let kind = if label == "rust.module.module" {
            SyntaxFactKind::Module
        } else {
            SyntaxFactKind::Declaration
        };
        let item = self.add(Some(parent), kind, label, start, start + anchor.len());
        let name_start = start + anchor.find(name).unwrap();
        self.add(
            Some(item),
            SyntaxFactKind::Occurrence,
            "rust.type_identifier.definition",
            name_start,
            name_start + name.len(),
        );
        item
    }
    fn implementation(&mut self, parent: u64, anchor: &str, target: &str) -> u64 {
        let start = self.text.find(anchor).unwrap();
        let scope = self.add(
            Some(parent),
            SyntaxFactKind::Scope,
            "rust.impl.scope",
            start,
            start + anchor.len(),
        );
        let target_start = start + anchor.find(target).unwrap();
        self.add(
            Some(scope),
            SyntaxFactKind::Signature,
            "rust.impl_type.scope_type",
            target_start,
            target_start + target.len(),
        );
        let function = "fn run() {}";
        let function_start = start + anchor.find(function).unwrap();
        let method = self.add(
            Some(scope),
            SyntaxFactKind::Declaration,
            "rust.function.declaration",
            function_start,
            function_start + function.len(),
        );
        self.add(
            Some(method),
            SyntaxFactKind::Occurrence,
            "rust.identifier.definition",
            function_start + 3,
            function_start + 6,
        );
        self.add(
            Some(method),
            SyntaxFactKind::Signature,
            "rust.body.signature",
            function_start + function.len() - 2,
            function_start + function.len(),
        );
        method
    }
    fn plan(&self) -> RustInherentOwnerPlan {
        rust_inherent_owner_plan(
            &self.facts,
            &self.source,
            self.text.as_bytes(),
            true,
            1024,
            64 * 1024,
            4096,
            64 * 1024 * 4096,
            256,
            1024 * 1024,
            &Cancellation::new(),
        )
        .unwrap()
    }
}

#[test]
fn forward_written_plain_struct_and_comment_trivia_keep_exact_authored_pair() {
    for source in [
        "struct Owner {}\nimpl Owner { fn run() {} }\n",
        "impl /* header */ Owner { fn run() {} }\npub struct Owner {}\n",
    ] {
        let mut fixture = Fixture::new(source);
        let anchor = if source.contains("pub struct") {
            "pub struct Owner {}"
        } else {
            "struct Owner {}"
        };
        let owner = fixture.item(1, anchor, "Owner", "rust.struct.declaration");
        let implementation = if source.contains("/* header */") {
            "impl /* header */ Owner { fn run() {} }"
        } else {
            "impl Owner { fn run() {} }"
        };
        let method = fixture.implementation(1, implementation, "Owner");
        let plan = fixture.plan();
        assert_eq!(plan.proofs.len(), 1);
        assert_eq!(
            (
                plan.proofs[0].method,
                plan.proofs[0].owner,
                plan.proofs[0].lexical_module
            ),
            (method, owner, None)
        );
        assert!(plan.unproven_methods.is_empty());
    }
}

#[test]
fn complete_namespace_census_rejects_duplicate_even_when_one_capture_is_omitted() {
    let source = "struct Owner {}\nstruct Owner { different: u8 }\nimpl Owner { fn run() {} }\n";
    for include_second in [false, true] {
        let mut fixture = Fixture::new(source);
        fixture.item(1, "struct Owner {}", "Owner", "rust.struct.declaration");
        if include_second {
            fixture.item(
                1,
                "struct Owner { different: u8 }",
                "Owner",
                "rust.struct.declaration",
            );
        }
        let method = fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
        let plan = fixture.plan();
        assert!(plan.proofs.is_empty());
        assert_eq!(plan.unproven_methods, [method]);
    }
}

#[test]
fn forged_comment_and_string_structs_never_supply_written_owner_authority() {
    for source in [
        "// struct Owner {}\nimpl Owner { fn run() {} }\n",
        "const TEXT: &str = r###\"struct Owner {}\"###;\nimpl Owner { fn run() {} }\n",
    ] {
        let mut fixture = Fixture::new(source);
        fixture.item(1, "struct Owner {}", "Owner", "rust.struct.declaration");
        fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
        assert!(fixture.plan().proofs.is_empty());
    }
}

#[test]
fn generic_path_where_trait_and_nested_targets_do_not_guess_plain_ownership() {
    for implementation in [
        "impl<T> Owner { fn run() {} }",
        "impl Owner where Owner: Copy { fn run() {} }",
        "impl Trait for Owner { fn run() {} }",
        "impl path::Owner { fn run() {} }",
    ] {
        let source = format!("struct Owner {{}}\n{implementation}\n");
        let mut fixture = Fixture::new(&source);
        fixture.item(1, "struct Owner {}", "Owner", "rust.struct.declaration");
        fixture.implementation(
            1,
            implementation,
            if implementation.contains("path::Owner") {
                "path::Owner"
            } else {
                "Owner"
            },
        );
        assert!(fixture.plan().proofs.is_empty(), "{implementation}");
    }
    let source = "fn outer() { struct Owner {} impl Owner { fn run() {} } }\n";
    let mut fixture = Fixture::new(source);
    let outer = fixture.add(
        Some(1),
        SyntaxFactKind::Declaration,
        "rust.function.declaration",
        0,
        source.trim_end().len(),
    );
    fixture.item(outer, "struct Owner {}", "Owner", "rust.struct.declaration");
    fixture.implementation(outer, "impl Owner { fn run() {} }", "Owner");
    assert!(fixture.plan().proofs.is_empty());
}

#[test]
fn module_local_identity_does_not_borrow_same_spelled_struct_from_a_sibling() {
    let source = "mod left { struct Owner {} }\nmod right { impl Owner { fn run() {} } }\n";
    let mut fixture = Fixture::new(source);
    let left = fixture.item(
        1,
        "mod left { struct Owner {} }",
        "left",
        "rust.module.module",
    );
    let right = fixture.item(
        1,
        "mod right { impl Owner { fn run() {} } }",
        "right",
        "rust.module.module",
    );
    fixture.item(left, "struct Owner {}", "Owner", "rust.struct.declaration");
    fixture.implementation(right, "impl Owner { fn run() {} }", "Owner");
    assert!(fixture.plan().proofs.is_empty());
}

#[test]
fn ordinary_default_derive_and_unrelated_simple_import_are_the_only_initial_wrappers() {
    let source = "use std::collections::VecDeque;\n#[derive(Default)]\npub struct Owner {}\nimpl Owner { fn run() {} }\n";
    let mut fixture = Fixture::new(source);
    fixture.item(1, "pub struct Owner {}", "Owner", "rust.struct.declaration");
    fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
    let import_end = source.find(';').unwrap() + 1;
    fixture.add(
        Some(1),
        SyntaxFactKind::Import,
        "rust.use.import",
        0,
        import_end,
    );
    let attribute_start = source.find("#[").unwrap();
    fixture.add(
        Some(1),
        SyntaxFactKind::Signature,
        "rust.attribute.signature",
        attribute_start,
        attribute_start + "#[derive(Default)]".len(),
    );
    assert_eq!(fixture.plan().proofs.len(), 1);
    for prefix in [
        "#[cfg(any())]\n",
        "#[rewrite]\n",
        "use other::Owner;\n",
        "use other::*;\n",
        "type Owner = Other;\n",
        "union Foreign { value: u8 }\n",
    ] {
        let source = format!("{prefix}struct Owner {{}}\nimpl Owner {{ fn run() {{}} }}\n");
        let mut fixture = Fixture::new(&source);
        fixture.item(1, "struct Owner {}", "Owner", "rust.struct.declaration");
        fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
        assert!(fixture.plan().proofs.is_empty(), "{prefix}");
    }
}

#[test]
fn source_hash_foreign_file_graph_and_incomplete_input_fail_closed() {
    let source = "struct Owner {}\nimpl Owner { fn run() {} }\n";
    let mut fixture = Fixture::new(source);
    fixture.item(1, "struct Owner {}", "Owner", "rust.struct.declaration");
    fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
    let stale = SourceRef::new(
        fixture.source.repository(),
        fixture.source.generation(),
        fixture.source.span(),
        content_hash(b"stale"),
        None,
    );
    assert!(
        rust_inherent_owner_plan(
            &fixture.facts,
            &stale,
            source.as_bytes(),
            true,
            1024,
            64 * 1024,
            4096,
            64 * 1024 * 4096,
            256,
            1024 * 1024,
            &Cancellation::new()
        )
        .is_err()
    );
    assert!(
        rust_inherent_owner_plan(
            &fixture.facts,
            &fixture.source,
            source.as_bytes(),
            false,
            1024,
            64 * 1024,
            4096,
            64 * 1024 * 4096,
            256,
            1024 * 1024,
            &Cancellation::new()
        )
        .unwrap()
        .proofs
        .is_empty()
    );
    fixture.facts[0] = SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Root,
        SourceSpan::new(FileId::from_bytes([8; 20]), 0, source.len() as u64).unwrap(),
        0,
        SyntaxKindLabel::new("rust.file.root").unwrap(),
    );
    assert!(
        rust_inherent_owner_plan(
            &fixture.facts,
            &fixture.source,
            source.as_bytes(),
            true,
            1024,
            64 * 1024,
            4096,
            64 * 1024 * 4096,
            256,
            1024 * 1024,
            &Cancellation::new()
        )
        .is_err()
    );
}

#[test]
fn declared_admission_accepts_large_sources_and_refuses_each_exact_one_less_bound() {
    let source = format!(
        "/*{}*/\npub struct Owner {{}}\nimpl Owner {{ fn run() {{}} }}\n",
        "padding ".repeat(600)
    );
    assert!(source.len() > rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES);
    let mut fixture = Fixture::new(&source);
    fixture.item(1, "pub struct Owner {}", "Owner", "rust.struct.declaration");
    fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
    let bytes = source.len();
    let facts = fixture.facts.len();
    let tokens = rust_source_tokens(&source, bytes, bytes)
        .unwrap()
        .unwrap()
        .len();
    let work = modeled_work(bytes, tokens, &fixture.facts).unwrap();
    let memory = bytes.checked_mul(128).unwrap() + facts.checked_mul(512).unwrap();
    let plan = |source_limit, token_limit, work_limit, memory_limit| {
        rust_inherent_owner_plan(
            &fixture.facts,
            &fixture.source,
            source.as_bytes(),
            true,
            facts,
            source_limit,
            token_limit,
            work_limit,
            256,
            memory_limit,
            &Cancellation::new(),
        )
        .unwrap()
    };
    assert_eq!(plan(bytes, tokens, work, memory).proofs.len(), 1);
    for refused in [
        plan(bytes - 1, tokens, work, memory),
        plan(bytes, tokens - 1, work, memory),
        plan(bytes, tokens, work - 1, memory),
        plan(bytes, tokens, work, memory - 1),
    ] {
        assert!(refused.proofs.is_empty());
    }
    assert!(
        rust_inherent_owner_plan(
            &fixture.facts,
            &fixture.source,
            source.as_bytes(),
            true,
            facts - 1,
            bytes,
            tokens,
            work,
            256,
            memory,
            &Cancellation::new()
        )
        .is_err()
    );
}

#[test]
fn direct_named_struct_fields_require_exact_name_type_extent_and_unique_written_name() {
    for (source, field_text, name, expected) in [
        (
            "pub struct Owner { pub value: Vec<u8> }",
            "pub value: Vec<u8>",
            "value",
            true,
        ),
        (
            "struct Owner { value: u8, value: u16 }",
            "value: u8",
            "value",
            false,
        ),
        (
            "struct Owner { actual: Pretend }",
            "actual: Pretend",
            "Pretend",
            false,
        ),
        (
            "struct Owner { /* hidden: u8 */ value: u8 }",
            "hidden: u8",
            "hidden",
            false,
        ),
        ("struct Owner { value: }", "value:", "value", false),
        (
            "struct Owner { callback: fn(value: u8) }",
            "value: u8",
            "value",
            false,
        ),
        (
            "struct Owner { nested: Wrapper<value: u8> }",
            "value: u8",
            "value",
            false,
        ),
        (
            "struct Owner { pub value: fn(u8, u16) -> Vec<u8>, other: u8 }",
            "pub value: fn(u8, u16) -> Vec<u8>",
            "value",
            true,
        ),
    ] {
        let mut fixture = Fixture::new(source);
        let owner = fixture.item(1, source, "Owner", "rust.struct.declaration");
        let start = source.find(field_text).unwrap();
        let field = fixture.add(
            Some(owner),
            SyntaxFactKind::Declaration,
            "rust.field.declaration",
            start,
            start + field_text.len(),
        );
        let named = start + field_text.find(name).unwrap();
        let definition = fixture.add(
            Some(field),
            SyntaxFactKind::Occurrence,
            "rust.field_identifier.definition",
            named,
            named + name.len(),
        );
        let plan = fixture.plan();
        assert_eq!(plan.fields.len(), usize::from(expected), "{source}");
        if expected {
            assert_eq!(
                (
                    plan.fields[0].field,
                    plan.fields[0].definition,
                    plan.fields[0].owner
                ),
                (field, definition, owner)
            );
        }
    }
}

#[test]
fn wrong_definition_kind_or_recovery_cannot_prove_a_direct_field() {
    let source = "struct Owner { value: u8 }";
    for recovery in [false, true] {
        let mut fixture = Fixture::new(source);
        let owner = fixture.item(1, source, "Owner", "rust.struct.declaration");
        let start = source.find("value").unwrap();
        let field = fixture.add(
            Some(owner),
            SyntaxFactKind::Declaration,
            "rust.field.declaration",
            start,
            start + "value: u8".len(),
        );
        fixture.add(
            Some(field),
            SyntaxFactKind::Occurrence,
            if recovery {
                "rust.field_identifier.definition"
            } else {
                "rust.type_identifier.definition"
            },
            start,
            start + 5,
        );
        if recovery {
            fixture.add(Some(1), SyntaxFactKind::ErrorRecovery, "rust.error", 0, 1);
        }
        assert!(fixture.plan().fields.is_empty());
    }
}

#[test]
fn wrong_method_or_owner_definition_kind_and_cancelled_proof_do_not_gain_ownership() {
    let source = "struct Owner {}\nimpl Owner { fn run() {} }\n";
    for forged in ["owner", "method"] {
        let mut fixture = Fixture::new(source);
        let owner = fixture.item(1, "struct Owner {}", "Owner", "rust.struct.declaration");
        let method = fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
        let fact = fixture
            .facts
            .iter_mut()
            .find(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.parent() == Some(if forged == "owner" { owner } else { method })
            })
            .unwrap();
        *fact = SyntaxFact::new(
            fact.local_id(),
            fact.parent(),
            fact.kind(),
            fact.span(),
            fact.depth(),
            SyntaxKindLabel::new(if forged == "owner" {
                "rust.identifier.definition"
            } else {
                "rust.type_identifier.definition"
            })
            .unwrap(),
        );
        assert!(fixture.plan().proofs.is_empty(), "{forged}");
    }
    let mut fixture = Fixture::new(source);
    fixture.item(1, "struct Owner {}", "Owner", "rust.struct.declaration");
    fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest));
    assert!(matches!(
        rust_inherent_owner_plan(
            &fixture.facts,
            &fixture.source,
            source.as_bytes(),
            true,
            1024,
            64 * 1024,
            4096,
            64 * 1024 * 4096,
            256,
            1024 * 1024,
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: rootlight_cancel::CancellationReason::ClientRequest
        })
    ));
}

fn deep_same_span_fixture() -> Fixture<'static> {
    let source = "struct Owner {}\nimpl Owner { fn run() {} }\n";
    let mut fixture = Fixture::new(source);
    fixture.item(1, "struct Owner {}", "Owner", "rust.struct.declaration");
    fixture.implementation(1, "impl Owner { fn run() {} }", "Owner");
    let mut scope = 1;
    for _ in 0..192 {
        scope = fixture.add(
            Some(scope),
            SyntaxFactKind::Scope,
            "rust.test.scope",
            0,
            source.len(),
        );
    }
    for _ in 0..64 {
        fixture.add(
            Some(scope),
            SyntaxFactKind::Declaration,
            "rust.function.declaration",
            0,
            source.len(),
        );
    }
    fixture
}

#[test]
fn deep_same_span_graph_admits_fact_depth_work_before_ancestry_candidate_collection() {
    let fixture = deep_same_span_fixture();
    let cancellation = Cancellation::new();
    Graph::new(&fixture.facts, &fixture.source, &cancellation).unwrap();
    let bytes = fixture.text.len();
    let tokens = rust_source_tokens(fixture.text, bytes, bytes)
        .unwrap()
        .unwrap()
        .len();
    let minimum = modeled_work(bytes, 0, &fixture.facts).unwrap();
    let complete = modeled_work(bytes, tokens, &fixture.facts).unwrap();
    assert!(minimum > bytes.checked_mul(bytes).unwrap());
    assert!(complete > minimum);
    let plan = |work| {
        rust_inherent_owner_plan(
            &fixture.facts,
            &fixture.source,
            fixture.text.as_bytes(),
            true,
            fixture.facts.len(),
            bytes,
            tokens,
            work,
            256,
            1024 * 1024,
            &cancellation,
        )
        .unwrap()
    };
    let before_ancestry = plan(minimum - 1);
    assert!(before_ancestry.proofs.is_empty());
    assert!(
        before_ancestry.unproven_methods.is_empty(),
        "fact/depth refusal precedes candidate ancestry walks"
    );
    for after_preflight in [plan(minimum), plan(complete - 1)] {
        assert!(after_preflight.proofs.is_empty());
        assert_eq!(
            after_preflight.unproven_methods.len(),
            1,
            "the token-aware check remains independent"
        );
    }
    assert_eq!(plan(complete).proofs.len(), 1);
}

#[test]
fn deep_ancestry_walk_observes_cancellation_after_an_admitted_parent_hop() {
    let fixture = deep_same_span_fixture();
    let cancellation = Cancellation::new();
    let graph = Graph::new(&fixture.facts, &fixture.source, &cancellation).unwrap();
    let leaf = fixture.facts.last().unwrap();
    let mut ancestors = graph.ancestors(leaf);
    assert!(ancestors.next().unwrap().is_ok());
    assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ParentCancelled));
    assert!(matches!(
        ancestors.next(),
        Some(Err(AdapterError::Cancelled {
            reason: rootlight_cancel::CancellationReason::ParentCancelled,
        }))
    ));
    assert!(ancestors.next().is_none());
    assert!(matches!(
        graph.module(leaf),
        Err(AdapterError::Cancelled {
            reason: rootlight_cancel::CancellationReason::ParentCancelled,
        })
    ));
}
