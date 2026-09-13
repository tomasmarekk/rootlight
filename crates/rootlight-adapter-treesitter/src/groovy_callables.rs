//! Lowers native callable captures without reparsing signature text.
//! Ownership follows the retained declaration hierarchy; nested closure parameters
//! cannot become parameters of their surrounding method.

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ir::{
    GroovyCallable, GroovyCallableDispatch as Dispatch, GroovyCallableVisibility as Visibility,
    SourceSpan,
};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
struct Header {
    modifiers: Option<(Dispatch, Visibility)>,
    name: Option<SourceSpan>,
    parameters: Option<SourceSpan>,
    parameter_shapes: BTreeMap<u64, (SourceSpan, bool, bool)>,
    annotated_parameter: bool,
    return_type: Option<rootlight_ir::GroovyCallableReturnType>,
}

/// Collects source-owned method shape once for a complete native fact stream.
///
/// # Errors
/// Returns cancellation or inconsistent capture ownership, duplicate header,
/// overlapping parameter, or count overflow errors.
pub(super) fn collect(
    facts: &[SyntaxFact],
    by_id: &HashMap<u64, &SyntaxFact>,
    cancellation: &Cancellation,
) -> Result<BTreeMap<u64, GroovyCallable>, AdapterError> {
    cancellation.check()?;
    let mut headers: BTreeMap<u64, Header> = BTreeMap::new();
    for fact in facts {
        cancellation.check()?;
        let kind = fact.syntax_kind().as_str();
        if !kind.starts_with("groovy.callable_")
            && !matches!(
                kind,
                "groovy.identifier.definition" | "groovy.quoted_identifier.definition"
            )
        {
            continue;
        }
        if !kind.starts_with("groovy.callable_")
            && !fact
                .parent()
                .and_then(|parent| by_id.get(&parent))
                .is_some_and(|parent| {
                    parent.syntax_kind().as_str() == "groovy.function.declaration"
                })
        {
            continue;
        }
        let Some(owner) = owner(fact, by_id, cancellation)? else {
            continue;
        };
        if !kind.starts_with("groovy.callable_") && fact.parent() != Some(owner) {
            continue;
        }
        let header = headers.entry(owner).or_default();
        if let Some(return_type) = return_type(kind) {
            if header.return_type.replace(return_type).is_some() {
                return Err(invalid());
            }
        } else if let Some(modifiers) = modifiers(kind) {
            if header.modifiers.replace(modifiers).is_some() {
                return Err(invalid());
            }
        } else if matches!(
            kind,
            "groovy.identifier.definition" | "groovy.quoted_identifier.definition"
        ) {
            if header.name.replace(fact.span()).is_some() {
                return Err(invalid());
            }
        } else if kind == "groovy.callable_parameters.expression" {
            if header.parameters.replace(fact.span()).is_some() {
                return Err(invalid());
            }
        } else if kind == "groovy.callable_annotation.expression" {
            header.annotated_parameter = true;
        } else if let Some((default, array)) = parameter(kind)
            && header
                .parameter_shapes
                .insert(fact.span().start_byte(), (fact.span(), default, array))
                .is_some()
        {
            return Err(invalid());
        }
    }
    let mut result = BTreeMap::new();
    for (owner, header) in headers {
        cancellation.check()?;
        let (Some((mut dispatch, mut visibility)), Some(name), Some(parameters)) =
            (header.modifiers, header.name, header.parameters)
        else {
            continue;
        };
        let parameter_count =
            u32::try_from(header.parameter_shapes.len()).map_err(|_| invalid())?;
        let mut required_parameter_count = 0u32;
        let mut last_end = parameters.start_byte();
        let mut variadic = false;
        for (span, default, array) in header.parameter_shapes.values() {
            cancellation.check()?;
            if span.file() != parameters.file()
                || span.start_byte() < last_end
                || span.start_byte() == span.end_byte()
                || span.end_byte() >= parameters.end_byte()
            {
                return Err(invalid());
            }
            last_end = span.end_byte();
            if !default {
                required_parameter_count = required_parameter_count
                    .checked_add(1)
                    .ok_or_else(invalid)?;
            }
            variadic = *array;
        }
        if header.annotated_parameter {
            dispatch = Dispatch::Unknown;
            visibility = Visibility::Unknown;
        }
        result.insert(
            owner,
            GroovyCallable {
                name,
                parameters,
                parameter_count,
                required_parameter_count,
                variadic,
                dispatch,
                visibility,
                return_type: header.return_type.unwrap_or_default(),
            },
        );
    }
    Ok(result)
}

fn owner(
    fact: &SyntaxFact,
    facts: &HashMap<u64, &SyntaxFact>,
    cancellation: &Cancellation,
) -> Result<Option<u64>, AdapterError> {
    let mut parent = fact.parent();
    for _ in 0..facts.len() {
        cancellation.check()?;
        let Some(id) = parent else {
            return Ok(None);
        };
        let ancestor = facts.get(&id).ok_or_else(invalid)?;
        if ancestor.syntax_kind().as_str() == "groovy.function.declaration" {
            return Ok(Some(id));
        }
        parent = ancestor.parent();
    }
    Err(invalid())
}

fn return_type(kind: &str) -> Option<rootlight_ir::GroovyCallableReturnType> {
    use rootlight_ir::GroovyCallableReturnType;
    Some(match kind {
        "groovy.callable_return_boolean.expression" => GroovyCallableReturnType::PrimitiveBoolean,
        "groovy.callable_return_other.expression" => GroovyCallableReturnType::Other,
        "groovy.callable_return_unknown.expression" => GroovyCallableReturnType::Unknown,
        _ => return None,
    })
}

fn modifiers(kind: &str) -> Option<(Dispatch, Visibility)> {
    Some(match kind {
        "groovy.callable_static_public.expression" => (Dispatch::Static, Visibility::Public),
        "groovy.callable_static_protected.expression" => (Dispatch::Static, Visibility::Protected),
        "groovy.callable_static_private.expression" => (Dispatch::Static, Visibility::Private),
        "groovy.callable_instance_public.expression" => (Dispatch::Instance, Visibility::Public),
        "groovy.callable_instance_protected.expression" => {
            (Dispatch::Instance, Visibility::Protected)
        }
        "groovy.callable_instance_private.expression" => (Dispatch::Instance, Visibility::Private),
        "groovy.callable_unknown.expression" => (Dispatch::Unknown, Visibility::Unknown),
        _ => return None,
    })
}

fn parameter(kind: &str) -> Option<(bool, bool)> {
    Some(match kind {
        "groovy.callable_parameter_required.expression" => (false, false),
        "groovy.callable_parameter_default.expression" => (true, false),
        "groovy.callable_parameter_required_array.expression" => (false, true),
        "groovy.callable_parameter_default_array.expression" => (true, true),
        _ => return None,
    })
}

fn invalid() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("groovy-callable-capture")
            .expect("built-in diagnostic code is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::{SyntaxFactKind, SyntaxKindLabel};
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;

    fn fact(id: u64, parent: Option<u64>, label: &str, start: u64, end: u64) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            if label.ends_with(".declaration") {
                SyntaxFactKind::Declaration
            } else {
                SyntaxFactKind::Signature
            },
            SourceSpan::new(FileId::from_bytes([7; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn facts() -> Vec<SyntaxFact> {
        vec![
            fact(1, None, "groovy.function.declaration", 0, 10),
            fact(
                2,
                Some(1),
                "groovy.callable_static_public.expression",
                0,
                10,
            ),
            fact(3, Some(1), "groovy.identifier.definition", 0, 1),
            fact(4, Some(1), "groovy.callable_parameters.expression", 1, 6),
            fact(
                5,
                Some(1),
                "groovy.callable_parameter_required.expression",
                2,
                3,
            ),
        ]
    }

    fn collected(
        facts: &[SyntaxFact],
        cancellation: &Cancellation,
    ) -> Result<BTreeMap<u64, GroovyCallable>, AdapterError> {
        collect(
            facts,
            &facts.iter().map(|fact| (fact.local_id(), fact)).collect(),
            cancellation,
        )
    }

    #[test]
    fn callable_return_capture_is_required_for_certainty_and_must_be_unique() {
        use rootlight_ir::GroovyCallableReturnType;
        let cancellation = Cancellation::new();
        let mut input = facts();
        assert_eq!(
            collected(&input, &cancellation).unwrap()[&1].return_type,
            GroovyCallableReturnType::Unknown
        );
        input.push(fact(
            7,
            Some(1),
            "groovy.callable_return_boolean.expression",
            0,
            10,
        ));
        assert_eq!(
            collected(&input, &cancellation).unwrap()[&1].return_type,
            GroovyCallableReturnType::PrimitiveBoolean
        );
        input.push(fact(
            8,
            Some(1),
            "groovy.callable_return_other.expression",
            0,
            10,
        ));
        assert!(collected(&input, &cancellation).is_err());
    }

    #[test]
    fn callable_collection_is_order_independent_and_cancellable() {
        let mut facts = facts();
        let cancellation = Cancellation::new();
        let before = collected(&facts, &cancellation).unwrap();
        assert_eq!(before.len(), 1);
        facts.reverse();
        assert_eq!(before, collected(&facts, &cancellation).unwrap());
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(collected(&facts, &cancellation).is_err());
        assert!(collected(&[], &cancellation).is_err());
    }

    #[test]
    fn callable_collection_rejects_inconsistent_ownership_and_parameters() {
        let cancellation = Cancellation::new();
        for extra in [
            fact(
                6,
                Some(1),
                "groovy.callable_static_public.expression",
                0,
                10,
            ),
            fact(
                6,
                Some(1),
                "groovy.callable_parameter_default.expression",
                2,
                4,
            ),
            fact(
                6,
                Some(1),
                "groovy.callable_parameter_required.expression",
                5,
                8,
            ),
            fact(
                6,
                Some(99),
                "groovy.callable_parameter_required.expression",
                4,
                5,
            ),
            fact(
                6,
                Some(6),
                "groovy.callable_parameter_required.expression",
                4,
                5,
            ),
        ] {
            let mut facts = facts();
            facts.push(extra);
            assert!(collected(&facts, &cancellation).is_err());
        }
        let mut missing_header = facts();
        missing_header.remove(1);
        assert!(
            collected(&missing_header, &cancellation)
                .unwrap()
                .is_empty()
        );
    }
}
