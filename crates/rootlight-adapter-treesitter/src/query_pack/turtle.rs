//! Retain native Turtle prefix evidence without promoting arbitrary RDF subjects.
//! Prefix signature captures also carry default bindings that have no written name.

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use crate::turtle_terms::{expanded, prefix_binding};
use rootlight_adapter_sdk::AdapterError;
use rootlight_adapter_sdk::SinkError;
use rootlight_cancel::Cancellation;
use std::collections::HashMap;
use tree_sitter::Node;

fn prefix_owner(node: Node<'_>) -> Option<Node<'_>> {
    let namespace = node
        .parent()
        .filter(|parent| parent.kind() == "namespace")?;
    namespace
        .parent()
        .filter(|parent| matches!(parent.kind(), "prefix_id" | "sparql_prefix"))
}

pub(super) fn candidate(
    context: &Context,
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    let resource_owner = if node.kind() == "subject" {
        node.parent()
    } else {
        Some(node)
    }
    .filter(|owner| owner.kind() == "triple");
    if let Some(owner) = resource_owner {
        let Some(syntax) = context.resources.get(&owner.id()).copied() else {
            return Ok(None);
        };
        if !matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition
        ) {
            return Err(query_failure("query-turtle-resource-role"));
        }
        return Ok(Some(QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role,
            syntax,
            required: true,
            native_depth: native_depth(node, cancel)?,
        }));
    }
    let owner = if role == StructuralRole::Definition {
        prefix_owner(node)
    } else {
        Some(node)
    };
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) {
        let Some(owner) = owner.filter(|owner| !owner.has_error() && !owner.is_missing()) else {
            return Ok(None);
        };
        let Some(text) = source
            .get(owner.byte_range())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        else {
            return Ok(None);
        };
        if !prefix_binding(text).is_some_and(|(name, iri)| !name.is_empty() && iri.is_some()) {
            return Ok(None);
        }
    }
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module => "turtle.file",
        StructuralRole::Declaration | StructuralRole::Definition => "turtle.prefix",
        StructuralRole::Signature if matches!(node.kind(), "prefix_id" | "sparql_prefix") => {
            "turtle.prefix_binding"
        }
        _ => return Err(query_failure("query-turtle-role")),
    };
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancel)?,
    }))
}

/// File-local proof is built in source order and never expands graph syntax.
pub(super) struct Context {
    resources: HashMap<usize, &'static str>,
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn is_iri(node: Node<'_>) -> bool {
    matches!(node.kind(), "iri_reference" | "prefixed_name")
}

fn resolved_is(text: &str, bindings: &HashMap<&str, Option<&str>>, expected: &str) -> bool {
    expanded(text, bindings)
        .is_some_and(|(prefix, local)| expected.strip_prefix(prefix) == Some(local))
}

impl Context {
    pub(super) fn new(
        root: Node<'_>,
        source: &[u8],
        cancel: &Cancellation,
        maximum: usize,
    ) -> Result<Option<Self>, AdapterError> {
        let mut result = Self {
            resources: HashMap::new(),
        };
        let mut bindings = HashMap::<&str, Option<&str>>::new();
        let mut remaining = maximum;
        let mut charge = || -> Result<bool, AdapterError> {
            cancel.check()?;
            let Some(left) = remaining.checked_sub(1) else {
                return Ok(false);
            };
            remaining = left;
            Ok(true)
        };
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if !charge()? {
                return Ok(None);
            }
            if child.is_error() || child.is_missing() {
                bindings.clear();
                continue;
            }
            if child.kind() == "directive" {
                let mut cursor = child.walk();
                for directive in child.named_children(&mut cursor) {
                    if !charge()? {
                        return Ok(None);
                    }
                    if !matches!(directive.kind(), "prefix_id" | "sparql_prefix") {
                        continue;
                    }
                    if directive.has_error() || directive.is_missing() {
                        bindings.clear();
                    } else if let Some((name, iri)) =
                        text(directive, source).and_then(prefix_binding)
                    {
                        bindings
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        bindings.insert(name, iri);
                    } else {
                        bindings.clear();
                    }
                }
                continue;
            }
            if child.kind() != "triple" || child.has_error() || child.is_missing() {
                continue;
            }
            let mut subject = None;
            let mut properties = None;
            let mut cursor = child.walk();
            for part in child.named_children(&mut cursor) {
                if !charge()? {
                    return Ok(None);
                }
                match part.kind() {
                    "subject" => subject = Some(part),
                    "property_list" => properties = Some(part),
                    _ => {}
                }
            }
            let (Some(subject), Some(properties)) = (subject, properties) else {
                continue;
            };
            let mut subject_term = None;
            let mut cursor = subject.walk();
            for term in subject.named_children(&mut cursor) {
                if !charge()? {
                    return Ok(None);
                }
                if is_iri(term) {
                    subject_term = Some(term)
                }
            }
            let Some(term) = subject_term else { continue };
            if text(term, source)
                .and_then(|name| expanded(name, &bindings))
                .is_none()
            {
                continue;
            }
            let mut class = false;
            let mut property = false;
            let mut cursor = properties.walk();
            for written in properties.named_children(&mut cursor) {
                if !charge()? {
                    return Ok(None);
                }
                if written.kind() != "property" {
                    continue;
                }
                let mut predicate = None;
                let mut objects = None;
                let mut cursor = written.walk();
                for part in written.named_children(&mut cursor) {
                    if !charge()? {
                        return Ok(None);
                    }
                    match part.kind() {
                        "predicate" => predicate = Some(part),
                        "object_list" => objects = Some(part),
                        _ => {}
                    }
                }
                let (Some(predicate), Some(objects)) = (predicate, objects) else {
                    continue;
                };
                let Some(predicate_text) = text(predicate, source) else {
                    continue;
                };
                if predicate_text != "a"
                    && !resolved_is(
                        predicate_text,
                        &bindings,
                        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                    )
                {
                    continue;
                }
                let mut cursor = objects.walk();
                for object in objects.named_children(&mut cursor) {
                    if !charge()? {
                        return Ok(None);
                    }
                    if !is_iri(object) {
                        continue;
                    }
                    let Some(name) = text(object, source) else {
                        continue;
                    };
                    class |= resolved_is(
                        name,
                        &bindings,
                        "http://www.w3.org/2000/01/rdf-schema#Class",
                    );
                    property |= resolved_is(
                        name,
                        &bindings,
                        "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property",
                    );
                }
            }
            // A resource with both metatypes needs a multi-role resource model.
            let syntax = match (class, property) {
                (true, false) => "turtle.class",
                (false, true) => "turtle.property",
                _ => continue,
            };
            result
                .resources
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.resources.insert(child.id(), syntax);
        }
        Ok(Some(result))
    }
}
