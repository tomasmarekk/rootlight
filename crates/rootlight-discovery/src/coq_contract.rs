//! Coq attribution is evidence for discovery, never a parser-support claim.

use super::*;

fn signals(source: &[u8], truncated: bool) -> Vec<LanguageSignal> {
    classify_prefix(
        &RelativePath::parse(Path::new("proof.v")).unwrap(),
        source,
        truncated,
        false,
    )
    .1
}

#[test]
fn coq_standalone_structure_keeps_named_declaration_evidence() {
    for source in [
        "Structure Pair := make_pair { field : nat }.",
        "#[local] Structure Pair : Type := make_pair { field : nat }.",
        "Local Structure Pair := { field : nat }.",
    ] {
        assert!(
            signals(source.as_bytes(), false)
                .iter()
                .any(|row| { row.language == "coq" && row.evidence == LanguageEvidence::Content }),
            "{source}"
        );
    }
}

#[test]
fn coq_standalone_declare_instance_requires_a_typed_name() {
    for source in [
        "Declare Instance promised : nat.",
        "#[local] Declare Instance promised : Known.Carrier.",
        "Local Declare Instance promised : (Known.Carrier nat).",
    ] {
        assert!(
            signals(source.as_bytes(), false)
                .iter()
                .any(|row| { row.language == "coq" && row.evidence == LanguageEvidence::Content }),
            "{source}"
        );
    }
}

#[test]
fn coq_standalone_existing_class_requires_one_qualified_reference() {
    for source in [
        "Existing Class Known.Chosen.",
        "#[local] Existing Class Chosen.",
        "Local Existing Class Known.Nested.Chosen.",
    ] {
        assert!(
            signals(source.as_bytes(), false)
                .iter()
                .any(|row| { row.language == "coq" && row.evidence == LanguageEvidence::Content }),
            "{source}"
        );
    }
}

#[test]
fn coq_standalone_new_heads_reject_missing_or_malformed_names() {
    for source in [
        "Structure := { field : nat }.",
        "Structure _ := { field : nat }.",
        "Structure Pair.",
        "Declare Instance : nat.",
        "Declare Instance _ : nat.",
        "Declare Instance Known.promised : nat.",
        "Declare Instance promised nat.",
        "Declare Instance promised : .",
        "Declare Instance promised := nat.",
        "Declare Instance promised :: nat.",
        "Existing Class.",
        "Existing Class _.",
        "Existing Class Known._.",
        "Existing Class Known..Chosen.",
        "Existing Class Known Chosen.",
        "Existing Class Known.Chosen := other.",
        "existing Class Known.Chosen.",
    ] {
        assert!(
            !signals(source.as_bytes(), false)
                .iter()
                .any(|row| row.language == "coq"),
            "{source}"
        );
    }
}

#[test]
fn coq_standalone_new_commands_preserve_lexical_and_prefix_boundaries() {
    for source in [
        "Structure Pair := make_pair { field : nat }.",
        "Declare Instance promised : nat.",
        "Existing Class Known.Chosen.",
    ] {
        for decoy in [
            format!("(* nested (* {source} *) note *)"),
            format!("\"{source}\""),
            format!("'{source}'"),
            format!("`{source}`"),
            format!("// {source}"),
            format!("/* {source} */"),
            format!("(* unfinished {source}"),
            format!(
                r#"module main
fn main() {{ println("say \". {source} ") }}"#
            ),
            format!(
                r#"module sample(input wire clk); initial $display("say \". {source} "); endmodule"#
            ),
        ] {
            assert!(
                !signals(decoy.as_bytes(), false)
                    .iter()
                    .any(|row| row.language == "coq"),
                "{decoy}"
            );
        }
        assert!(
            !signals(source.as_bytes(), true)
                .iter()
                .any(|row| row.language == "coq"),
            "{source}"
        );
        let complete = format!("{source} ");
        assert!(
            signals(complete.as_bytes(), true)
                .iter()
                .any(|row| row.language == "coq"),
            "{complete}"
        );
        let incomplete = source.strip_suffix('.').unwrap();
        assert!(
            !signals(incomplete.as_bytes(), false)
                .iter()
                .any(|row| row.language == "coq"),
            "{incomplete}"
        );
        let conflict = format!("{source}\nmodule sample(input wire clk); endmodule");
        let rows = signals(conflict.as_bytes(), false);
        for language in ["coq", "verilog"] {
            assert!(
                rows.iter().any(|row| {
                    row.language == language && row.evidence == LanguageEvidence::Content
                }),
                "{language}: {rows:?}"
            );
        }
    }
}

#[test]
fn coq_alias_advertises_structural_analysis_without_a_suffix_default() {
    assert_eq!(canonical_language("rocq"), Some("coq"));
    assert_eq!(canonical_language("coq"), Some("coq"));
    let capability = language_capabilities()
        .iter()
        .find(|row| row.language == "coq")
        .unwrap();
    assert!(capability.suffixes.is_empty());
    assert_eq!(capability.analyzers, ["treesitter"]);
    assert_eq!(capability.maximum_tier, "tier_d");
    assert_eq!(extension_language("proof.v"), None);
}

#[test]
fn coq_commands_require_written_complete_sentence_evidence() {
    for source in [
        "From Stdlib Require Import List Nat.",
        "Require Export Corelib.Init.Prelude.",
        "Declare ML Module \"example.plugin\".",
        r#"Definition path : string := "\"."#,
        r#"Check "\". Definition value : string := "text"."#,
        r#"Definition text : string := "a ""quoted"" value"."#,
        "Definition π' : nat := 1.",
        "#[local, universes(polymorphic)] Definition sample := Nat.add.",
        "Local Program Definition sample : nat := 1.",
        "Lemma sample : True. Proof. exact I. Qed.",
        "Fixpoint count (n : nat) := match n with O => O | S p => count p end.",
        "Inductive sample : Type := | First | Second.",
        "Record sample := { field : nat }.",
        "Module Type Sample. Parameter field : nat. End Sample.",
        "Section Sample. Context {T : Type}. End Sample.",
        "Ltac sample := exact I.",
        "Notation \"a +++ b\" := (Nat.add a b).",
        "(* nested (* Definition fake := 0. *) note *) Definition sample := 1.",
    ] {
        assert!(
            signals(source.as_bytes(), false)
                .iter()
                .any(|row| { row.language == "coq" && row.evidence == LanguageEvidence::Content }),
            "{source}"
        );
    }
}

#[test]
fn coq_comments_strings_and_foreign_code_do_not_supply_commands() {
    for source in [
        "",
        "(* Definition hidden := 1. *)",
        "(* Declare ML Module \"hidden.plugin\". *)",
        "\"Declare ML Module \"\"hidden.plugin\"\".\"",
        "(* nested (* Lemma hidden : True. *) *)",
        "(* \"*)\" Definition hidden := 1. *)",
        "(* \"unterminated *)\nDefinition hidden := 1.",
        "\"Definition hidden := 1.\"",
        "\"escaped \"\" Definition hidden := 1.\"",
        "'Definition hidden := 1.'",
        "`Definition hidden := 1.`",
        "// Definition hidden := 1.",
        "/* Definition hidden := 1. */",
        "module main\nfn main() { println('Definition hidden := 1.') }",
        "module sample(input wire clk); endmodule",
        "definition sample := 1.",
        "Definition sample",
        "Definition sample := 1",
        "Declare ML Module \"incomplete.plugin\"",
    ] {
        assert!(
            !signals(source.as_bytes(), false)
                .iter()
                .any(|row| row.language == "coq"),
            "{source}"
        );
    }
}

#[test]
fn coq_and_hdl_conflicting_evidence_is_preserved() {
    for source in [
        "Definition sample := 1.\nmodule sample(input wire clk); endmodule",
        "module sample(input wire clk); endmodule\nDefinition sample := 1.",
        r#"module sample(input wire clk); initial $display("say \". Definition hidden := 1. "); endmodule
Definition sample := 1."#,
    ] {
        let rows = signals(source.as_bytes(), false);
        for language in ["coq", "verilog"] {
            assert!(
                rows.iter().any(
                    |row| row.language == language && row.evidence == LanguageEvidence::Content
                ),
                "{language}: {rows:?}"
            );
        }
    }
}

#[test]
fn coq_escaped_foreign_strings_do_not_supply_command_heads() {
    for (source, language) in [
        (
            r#"module sample(input wire clk); initial $display("say \". Definition hidden := 1. "); endmodule"#,
            "verilog",
        ),
        (
            r#"module main
fn main() { println("say \". Definition hidden := 1. ") }"#,
            "v",
        ),
        (
            r#"module main
const payload = "say \". Definition hidden := 1. "
fn main() {}"#,
            "v",
        ),
    ] {
        let rows = signals(source.as_bytes(), false);
        assert!(
            rows.iter().any(|row| {
                row.language == language && row.evidence == LanguageEvidence::Content
            }),
            "{source}: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.language == "coq"),
            "{source}: {rows:?}"
        );
    }
}

#[test]
fn coq_truncated_prefix_keeps_only_a_complete_prior_sentence() {
    for (source, expected) in [
        ("Definition value := 1.", false),
        ("Definition value := 1. ", true),
        ("Definition value := Nat.", false),
        ("Definition value := Nat", false),
        ("Definition prior := 1.\n(* unfinished", true),
        ("(* unfinished Definition hidden := 1. ", false),
        (
            r#"module sample(input wire clk); initial $display("say \". Definition hidden := 1. "#,
            false,
        ),
    ] {
        assert_eq!(
            signals(source.as_bytes(), true)
                .iter()
                .any(|row| row.language == "coq"),
            expected,
            "{source}"
        );
    }
}

#[test]
fn coq_decoded_discovery_preserves_original_sources_and_overrides() {
    use rootlight_config::{ConfigLayer, ConfigSource};
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = "(* λ😀 *)\nDefinition value' : nat := 1.\n";
    let mut cases = Vec::new();
    for (index, encoding) in ["utf-8", "utf-16le", "utf-16be"].into_iter().enumerate() {
        let bytes = if index == 0 {
            source.as_bytes().to_vec()
        } else {
            let mut bytes = if index == 1 {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in source.encode_utf16() {
                bytes.extend(if index == 1 {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            bytes
        };
        let path = format!("proof{index}.v");
        std::fs::write(temporary.path().join(&path), &bytes).unwrap();
        cases.push((path, bytes, encoding));
    }
    std::fs::write(temporary.path().join("override.v"), source).unwrap();
    let config = ConfigSnapshot::resolve(&[ConfigLayer {
        source: ConfigSource::User,
        contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'override.v'='python'\n",
    }]).unwrap();
    let repository = rootlight_ids::derive_repository(b"coq-encoded-discovery").id();
    let root = RepositoryRoot::open(repository, temporary.path()).unwrap();
    let result = discover_with_snapshots(
        &root,
        &config,
        &DiscoveryPolicy::build(Vec::new(), true).unwrap(),
        DiscoveryLimits::from_config(&config),
        BTreeMap::new(),
        &Cancellation::new(),
    )
    .unwrap();
    assert!(result.manifest.coverage.complete);
    assert!(result.manifest.exclusions.is_empty());
    assert_eq!(result.manifest.inputs.len(), 4);
    for (path, bytes, encoding) in cases {
        let input = result
            .manifest
            .inputs
            .iter()
            .find(|row| row.path == path)
            .unwrap();
        assert_eq!(input.encoding, encoding);
        assert_eq!(input.content_hash, content_hash(&bytes));
        assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(result.snapshots[&input.file].content(), bytes);
        assert_eq!(
            input.language_signals,
            [LanguageSignal {
                language: "coq".to_owned(),
                evidence: LanguageEvidence::Content,
            }]
        );
    }
    let overridden = result
        .manifest
        .inputs
        .iter()
        .find(|row| row.path == "override.v")
        .unwrap();
    assert!(
        overridden
            .language_signals
            .iter()
            .any(|row| row.language == "python" && row.evidence == LanguageEvidence::Configuration)
    );
}
