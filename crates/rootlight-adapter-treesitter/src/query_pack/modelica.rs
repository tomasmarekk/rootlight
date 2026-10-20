//! Native function/input ownership is separate from general Modelica semantics.
//! A bounded declaration census prevents local/duplicate names from proving input reads.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn text<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn bare<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if node.kind() != "IDENT" || node.has_error() || node.is_missing() {
        return None;
    }
    let name = text(node, source)?;
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    ((first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(name)
}

fn function<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    if node.kind() != "ClassDefinition"
        || node.has_error()
        || node.is_missing()
        || node.parent()?.kind() != "ClassDefinitionClause"
        || node.parent()?.parent()?.kind() != "StoredDefinition"
    {
        return None;
    }
    let prefixes = node.child_by_field_name("classPrefixes")?;
    if prefixes.child_by_field_name("function").is_none()
        || prefixes.child_by_field_name("partial").is_some()
        || prefixes.child_by_field_name("operator").is_some()
    {
        return None;
    }
    let specifier = node.child_by_field_name("classSpecifier")?;
    if specifier.kind() != "LongClassSpecifier"
        || specifier.child_by_field_name("extends").is_some()
    {
        return None;
    }
    let name = specifier.child_by_field_name("identifier")?;
    (bare(name, source)? == bare(specifier.child_by_field_name("endIdentifier")?, source)?)
        .then_some(name)
}

fn owner<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut current = node.parent();
    while let Some(parent) = current {
        cancel.check()?;
        if parent.kind() == "ClassDefinition" {
            return Ok(function(parent, source).map(|_| parent));
        }
        current = parent.parent();
    }
    Ok(None)
}

fn input<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancel.check()?;
    if bare(node, source).is_none() {
        return Ok(None);
    }
    let Some(declaration) = node.parent().filter(|node| node.kind() == "Declaration") else {
        return Ok(None);
    };
    if declaration.child_by_field_name("identifier") != Some(node) {
        return Ok(None);
    }
    let Some(component) = declaration
        .parent()
        .filter(|node| node.kind() == "ComponentDeclaration")
        .and_then(|node| node.parent())
        .filter(|node| node.kind() == "ComponentClause")
    else {
        return Ok(None);
    };
    if component.child_by_field_name("input").is_none() {
        return Ok(None);
    }
    let Some(element) = component
        .parent()
        .filter(|node| node.kind() == "NamedElementClause")
    else {
        return Ok(None);
    };
    if ["redeclare", "inner", "outer", "replaceable"]
        .iter()
        .any(|field| element.child_by_field_name(field).is_some())
    {
        return Ok(None);
    }
    Ok(owner(node, source, cancel)?.map(|_| declaration))
}

#[derive(Default)]
struct FunctionBindings<'s> {
    names: HashMap<&'s str, (usize, bool)>,
    barrier: bool,
}

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    functions: HashMap<usize, FunctionBindings<'s>>,
    maximum: usize,
    initialized: bool,
    complete: bool,
    exhausted: bool,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize) -> Self {
        Self {
            source,
            functions: HashMap::new(),
            maximum,
            initialized: false,
            complete: false,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn initialize(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        self.initialized = true;
        while let Some(parent) = node.parent() {
            cancel.check()?;
            node = parent;
        }
        if node.kind() != "StoredDefinition" || node.has_error() {
            return Ok(());
        }
        let mut cursor = node.walk();
        let mut visited = 0;
        loop {
            cancel.check()?;
            if visited >= self.maximum {
                self.exhausted = true;
                return Ok(());
            }
            visited += 1;
            let current = cursor.node();
            if let Some(function) = owner(current, self.source, cancel)? {
                self.functions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let bindings = self.functions.entry(function.start_byte()).or_default();
                if current.kind() == "Declaration" {
                    if let Some(name_node) = current.child_by_field_name("identifier")
                        && let Some(name) = bare(name_node, self.source)
                    {
                        let is_input = input(name_node, self.source, cancel)?.is_some();
                        bindings
                            .names
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        let value = bindings.names.entry(name).or_insert((0, true));
                        value.0 = value
                            .0
                            .checked_add(1)
                            .ok_or(SinkError::AccountingOverflow)?;
                        value.1 &= is_input;
                    } else {
                        bindings.barrier = true;
                    }
                }
                if current.kind() == "ClassDefinition"
                    || current.kind() == "ExtendsClause"
                    || (current.kind() == "NamedElementClause"
                        && ["redeclare", "inner", "outer", "replaceable"]
                            .iter()
                            .any(|field| current.child_by_field_name(field).is_some()))
                {
                    bindings.barrier = true;
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    self.complete = true;
                    return Ok(());
                }
            }
        }
    }

    fn reference(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<&'static str, AdapterError> {
        let Some(name_node) = node.child_by_field_name("identifier") else {
            return Ok("modelica.unresolved_reference");
        };
        let Some(name) = bare(name_node, self.source) else {
            return Ok("modelica.unresolved_reference");
        };
        let Some(reference) = node
            .parent()
            .filter(|node| node.kind() == "ComponentReference")
        else {
            return Ok("modelica.unresolved_reference");
        };
        if reference.named_child_count() != 1
            || reference.child_by_field_name("global").is_some()
            || node.child_by_field_name("arraySubscripts").is_some()
        {
            return Ok("modelica.unresolved_reference");
        }
        let mut current = reference.parent();
        let mut rhs = false;
        let mut scoped = true;
        while let Some(parent) = current {
            cancel.check()?;
            if parent.kind() == "FunctionCall"
                && parent
                    .child_by_field_name("functionReference")
                    .is_some_and(|head| {
                        head.start_byte() <= node.start_byte() && node.end_byte() <= head.end_byte()
                    })
            {
                return Ok("modelica.unresolved_reference");
            }
            if matches!(
                parent.kind(),
                "ForStatement" | "ForEquation" | "ForIndices" | "ComprehensionClause"
            ) {
                scoped = false;
            }
            if parent.kind() == "SimpleAssignmentStatement" {
                rhs = parent
                    .child_by_field_name("expression")
                    .is_some_and(|expression| {
                        expression.start_byte() <= node.start_byte()
                            && node.end_byte() <= expression.end_byte()
                    });
            }
            if parent.kind() == "ClassDefinition" {
                if !rhs || function(parent, self.source).is_none() {
                    return Ok("modelica.unresolved_reference");
                }
                if !self.initialized {
                    self.initialize(node, cancel)?;
                }
                let proven = self.complete
                    && scoped
                    && self
                        .functions
                        .get(&parent.start_byte())
                        .is_some_and(|bindings| {
                            !bindings.barrier && bindings.names.get(name) == Some(&(1, true))
                        });
                return Ok(if proven {
                    "modelica.input_read"
                } else {
                    "modelica.unresolved_read"
                });
            }
            current = parent.parent();
        }
        Ok("modelica.unresolved_reference")
    }
}

/// Retain only native owners and exact leaves; unproved semantics stay explicit.
/// Cache admission, allocation and cancellation failures propagate to the parser.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    let mut range = node.byte_range();
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module => {
            range = 0..source.len();
            "modelica.file"
        }
        StructuralRole::Scope if node.kind() == "StoredDefinition" => {
            range = 0..source.len();
            "modelica.file"
        }
        StructuralRole::Scope
        | StructuralRole::Declaration
        | StructuralRole::Definition
        | StructuralRole::Signature => {
            if let Some(name) = function(node, source) {
                if role == StructuralRole::Definition {
                    range = name.byte_range();
                }
                if role == StructuralRole::Signature {
                    range = node.start_byte()..name.end_byte();
                }
                if role == StructuralRole::Definition {
                    "modelica.name"
                } else if role == StructuralRole::Signature {
                    "modelica.header"
                } else {
                    "modelica.function"
                }
            } else if matches!(
                role,
                StructuralRole::Declaration | StructuralRole::Definition
            ) && let Some(declaration) = input(node, source, cancel)?
            {
                if role == StructuralRole::Declaration {
                    range = declaration.byte_range();
                }
                if role == StructuralRole::Definition {
                    "modelica.name"
                } else {
                    "modelica.input_parameter"
                }
            } else {
                return Ok(None);
            }
        }
        StructuralRole::Reference => {
            let Some(name) = node.child_by_field_name("identifier") else {
                return Ok(None);
            };
            range = name.byte_range();
            cache.reference(node, cancel)?
        }
        StructuralRole::Expression => {
            if node.kind() == "ClassDefinition" && function(node, source).is_some() {
                return Ok(None);
            }
            "modelica.unsupported"
        }
        StructuralRole::Comment => "modelica.comment",
        StructuralRole::StringLiteral => "modelica.string",
        _ => return Err(query_failure("query-modelica-role")),
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancel)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn native_census_admission_and_cancellation_cannot_publish_input_certification() {
        let source = b"function f input Real x; output Real y; algorithm y := x; end f;";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_modelica::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        let read = loop {
            let node = cursor.node();
            if node.kind() == "ComponentReferencePart" && text(node, source) == Some("x") {
                break node;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                assert!(cursor.goto_parent(), "native input read is present");
            }
        };
        let mut bounded = CaptureCache::new(source, 1);
        assert_eq!(
            bounded.reference(read, &Cancellation::new()).unwrap(),
            "modelica.unresolved_read"
        );
        assert!(bounded.exhausted());
        let mut complete = CaptureCache::new(source, 4096);
        assert_eq!(
            complete.reference(read, &Cancellation::new()).unwrap(),
            "modelica.input_read"
        );
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            candidate(
                read,
                StructuralRole::Reference,
                source,
                &cancelled,
                &mut complete
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
