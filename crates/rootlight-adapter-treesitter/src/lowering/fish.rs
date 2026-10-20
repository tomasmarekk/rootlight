//! Written Fish functions and named arguments support physical containment.
//! Expansion, event execution and runtime command lookup remain explicit gaps.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "fish.function.declaration")
            | (EntityKind::Parameter, "fish.parameter.declaration")
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let root =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "fish.file.module";
    let unsupported = fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "fish.unsupported.expression";
    [
        root.then_some((
            FactDomain::Entities,
            "fish-expanded-conditional-and-runtime-declarations-unavailable",
        )),
        root.then_some((
            FactDomain::Occurrences,
            "fish-general-variable-and-command-occurrences-unavailable",
        )),
        root.then_some((
            FactDomain::Relations,
            "fish-runtime-command-source-eval-and-rebinding-unavailable",
        )),
        unsupported.then_some((
            FactDomain::Entities,
            "fish-function-name-header-or-execution-context-unavailable",
        )),
    ]
    .into_iter()
    .flatten()
}
