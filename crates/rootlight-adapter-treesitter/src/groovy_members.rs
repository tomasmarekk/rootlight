//! Retains native field and property modifiers for each authored declarator.
//! Sibling bindings share their statement header without borrowing initializer tokens.

use std::collections::{BTreeMap, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ir::{EntityVisibility, GroovyMember, GroovyMemberStorage, SourceSpan};

/// Collects per-declarator metadata from native statement headers and type owners.
///
/// # Errors
/// Returns cancellation, duplicate captures or inconsistent native ancestry.
pub(super) fn collect(
    facts: &[SyntaxFact],
    by_id: &HashMap<u64, &SyntaxFact>,
    bindings: &crate::groovy_bindings::GroovyBindings<'_>,
    cancellation: &Cancellation,
) -> Result<BTreeMap<u64, (SourceSpan, GroovyMember)>, AdapterError> {
    cancellation.check()?;
    let mut headers = BTreeMap::new();
    let mut names = BTreeMap::new();
    for fact in facts {
        cancellation.check()?;
        if let Some(header) = modifiers(fact.syntax_kind().as_str()) {
            if headers.insert(fact.span(), header).is_some() {
                return Err(invalid());
            }
        } else if fact.syntax_kind().as_str() == "groovy.identifier.definition"
            && let Some(parent) = fact.parent().and_then(|id| by_id.get(&id))
            && is_member(parent)
            && names.insert(parent.local_id(), fact.span()).is_some()
        {
            return Err(invalid());
        }
    }
    let mut result = BTreeMap::new();
    for fact in facts.iter().filter(|fact| is_member(fact)) {
        cancellation.check()?;
        let Some(name) = names.get(&fact.local_id()).copied() else {
            continue;
        };
        let Some(owner) = bindings.enclosing_type(fact, cancellation)? else {
            continue;
        };
        let mut parent = fact.parent();
        for _ in 0..facts.len() {
            cancellation.check()?;
            let Some(id) = parent else { break };
            let ancestor = by_id.get(&id).ok_or_else(invalid)?;
            if let Some((storage, visibility)) = headers.get(&ancestor.span()) {
                result.insert(
                    fact.local_id(),
                    (
                        ancestor.span(),
                        GroovyMember {
                            owner,
                            declaration: fact.span(),
                            name,
                            storage: *storage,
                            visibility: *visibility,
                        },
                    ),
                );
                break;
            }
            parent = ancestor.parent();
        }
    }
    Ok(result)
}

fn is_member(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "groovy.field.declaration" | "groovy.property.declaration"
    )
}

fn modifiers(kind: &str) -> Option<(GroovyMemberStorage, EntityVisibility)> {
    use EntityVisibility::{Private, Public, Restricted, Unknown};
    use GroovyMemberStorage::{Instance, Static};
    Some(match kind {
        "groovy.member_static_public.expression" => (Static, Public),
        "groovy.member_static_protected.expression" => (Static, Restricted),
        "groovy.member_static_private.expression" => (Static, Private),
        "groovy.member_instance_public.expression" => (Instance, Public),
        "groovy.member_instance_protected.expression" => (Instance, Restricted),
        "groovy.member_instance_private.expression" => (Instance, Private),
        "groovy.member_unknown.expression" => (GroovyMemberStorage::Unknown, Unknown),
        _ => return None,
    })
}

fn invalid() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("groovy-member-native-evidence")
            .expect("fixed diagnostic code is valid"),
    }
}
