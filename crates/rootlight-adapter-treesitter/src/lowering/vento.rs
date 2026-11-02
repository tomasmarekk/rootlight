//! Vento host declarations remain distinct from opaque JavaScript and rendering.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain};

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "vento.function.declaration")
            | (EntityKind::Parameter, "vento.parameter.declaration")
            | (EntityKind::Variable, "vento.variable.declaration")
    )
}

pub(super) fn reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "vento.filter_name.reference"
}

pub(super) fn gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if fact.kind() != SyntaxFactKind::Signature {
        return None;
    }
    Some(match fact.syntax_kind().as_str() {
        "vento.invalid_file.signature" => (
            FactDomain::Entities,
            "vento-complete-native-host-syntax-unavailable",
        ),
        "vento.javascript.signature" => (
            FactDomain::Relations,
            "vento-javascript-expression-binding-unavailable",
        ),
        "vento.filter.signature" => (
            FactDomain::Relations,
            "vento-filter-host-binding-unavailable",
        ),
        "vento.output_text.signature" => (
            FactDomain::Entities,
            "vento-output-markup-structure-unavailable",
        ),
        "vento.unmodeled_host.signature" => (
            FactDomain::Relations,
            "vento-native-host-construct-not-lowered",
        ),
        _ => return None,
    })
}
