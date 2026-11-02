//! Keep GYP written build targets distinct from evaluated build configuration.
//! Literal source-path uses retain physical evidence without inventing file
//! targets, dependency edges, include resolution or command execution.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain};

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    kind == EntityKind::BuildTarget && label == "gyp.target.declaration"
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "gyp.source_path.reference"
}

pub(super) fn gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if fact.kind() != SyntaxFactKind::Signature {
        return None;
    }
    Some((
        FactDomain::Relations,
        match fact.syntax_kind().as_str() {
            "gyp.invalid_file.signature" => "gyp-complete-literal-data-file-unavailable",
            "gyp.duplicate_key.signature" => "gyp-duplicate-dictionary-keys-are-not-evaluated",
            "gyp.targets_unavailable.signature" | "gyp.target_name_unavailable.signature" => {
                "gyp-literal-target-declaration-unavailable"
            }
            "gyp.duplicate_target.signature" => {
                "gyp-duplicate-written-target-names-remain-distinct-without-evaluated-uniqueness"
            }
            "gyp.conditions.signature" => "gyp-conditional-configuration-not-evaluated",
            "gyp.includes.signature" => "gyp-includes-and-command-expansion-not-resolved",
            "gyp.dependencies.signature" => {
                "gyp-target-dependencies-and-command-expansion-not-resolved"
            }
            "gyp.expansion.signature" => "gyp-plain-literal-source-path-unavailable",
            "gyp.settings.signature" => "gyp-other-build-settings-and-runtime-effects-not-modeled",
            _ => return None,
        },
    ))
}
