//! Native JSON ancestry establishes schema contexts before any semantic capture.
//! All member addresses survive independently; declarations and local references
//! require an explicit supported dialect, unambiguous keys and resource proof.

use std::collections::HashSet;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use crate::json_schema_pointer::{
    PointerError, Work, absolute_uri, decode_string, fragment_tokens, resolve_native,
};

use super::{QueryCandidate, StructuralRole, native_depth};

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

fn root<'tree>(mut node: Node<'tree>, work: &mut Work<'_>) -> Result<Node<'tree>, PointerError> {
    while let Some(parent) = node.parent() {
        work.step()?;
        node = parent;
    }
    Ok(node)
}

fn text<'s>(node: Node<'_>, source: &'s [u8]) -> Result<&'s str, PointerError> {
    source
        .get(node.byte_range())
        .and_then(|raw| std::str::from_utf8(raw).ok())
        .ok_or(PointerError::Barrier(
            "json-schema-source-range-unavailable",
        ))
}

fn key(node: Node<'_>, source: &[u8], work: &mut Work<'_>) -> Result<String, PointerError> {
    let key = node
        .child_by_field_name("key")
        .ok_or(PointerError::Barrier("json-schema-member-key-unavailable"))?;
    if key.kind() != "string" || key.has_error() || key.is_missing() {
        return Err(PointerError::Barrier("json-schema-member-key-unavailable"));
    }
    decode_string(text(key, source)?, work)
}

fn member<'tree>(
    node: Node<'tree>,
    wanted: &str,
    source: &[u8],
    work: &mut Work<'_>,
) -> Result<Option<Node<'tree>>, PointerError> {
    if node.kind() != "object" || node.has_error() || node.is_missing() {
        return Err(PointerError::Barrier("json-schema-invalid-schema-object"));
    }
    let mut names = HashSet::new();
    let mut result = None;
    let mut cursor = node.walk();
    for pair in node.named_children(&mut cursor) {
        work.step()?;
        if pair.kind() != "pair" {
            return Err(PointerError::Barrier("json-schema-invalid-schema-object"));
        }
        let name = key(pair, source, work)?;
        names
            .try_reserve(1)
            .map_err(|_| AdapterError::from(SinkError::AllocationFailed))?;
        if names.contains(&name) {
            return Err(PointerError::Barrier("json-schema-ambiguous-schema-object"));
        }
        if name == wanted {
            result = pair.child_by_field_name("value");
        }
        names.insert(name);
    }
    Ok(result)
}

fn single_schema(keyword: &str) -> bool {
    matches!(
        keyword,
        "items"
            | "contains"
            | "additionalProperties"
            | "propertyNames"
            | "not"
            | "if"
            | "then"
            | "else"
            | "unevaluatedItems"
            | "unevaluatedProperties"
            | "contentSchema"
    )
}

fn map_schema(keyword: &str) -> bool {
    matches!(
        keyword,
        "$defs" | "properties" | "patternProperties" | "dependentSchemas"
    )
}

fn array_schema(keyword: &str) -> bool {
    matches!(keyword, "allOf" | "anyOf" | "oneOf" | "prefixItems")
}

#[derive(Clone, Copy)]
struct Context {
    schema: bool,
    resource_barrier: bool,
}

fn context(
    mut node: Node<'_>,
    document_root: Node<'_>,
    source: &[u8],
    work: &mut Work<'_>,
) -> Result<Context, PointerError> {
    let mut resource_barrier = false;
    loop {
        work.step()?;
        if !matches!(node.kind(), "object" | "true" | "false")
            || node.has_error()
            || node.is_missing()
        {
            return Ok(Context {
                schema: false,
                resource_barrier,
            });
        }
        if node.kind() == "object" {
            if node != document_root && member(node, "$id", source, work)?.is_some() {
                resource_barrier = true;
            }
            if let Some(dialect) = member(node, "$schema", source, work)?
                && decode_string(text(dialect, source)?, work)? != DIALECT
            {
                return Err(PointerError::Barrier(
                    "json-schema-unsupported-nested-dialect",
                ));
            }
        }
        if node == document_root {
            return Ok(Context {
                schema: true,
                resource_barrier,
            });
        }
        let Some(parent) = node.parent() else {
            return Ok(Context {
                schema: false,
                resource_barrier,
            });
        };
        if parent.kind() == "pair" && parent.child_by_field_name("value") == Some(node) {
            let Some(object) = parent.parent() else {
                return Ok(Context {
                    schema: false,
                    resource_barrier,
                });
            };
            let incoming = key(parent, source, work)?;
            if let Some(map) = object.parent().filter(|parent| parent.kind() == "pair")
                && map.child_by_field_name("value") == Some(object)
                && map_schema(&key(map, source, work)?)
                && let Some(owner) = map.parent()
            {
                // A schema-map member name may itself spell an applicator keyword.
                // Duplicate names in this exact schema map invalidate its paths.
                member(object, &incoming, source, work)?;
                node = owner;
                continue;
            }
            if single_schema(&incoming) {
                node = object;
                continue;
            }
        } else if parent.kind() == "array"
            && let Some(array) = parent.parent().filter(|parent| parent.kind() == "pair")
            && array.child_by_field_name("value") == Some(parent)
            && array_schema(&key(array, source, work)?)
            && let Some(owner) = array.parent()
        {
            node = owner;
            continue;
        }
        return Ok(Context {
            schema: false,
            resource_barrier,
        });
    }
}

fn declaration(
    pair: Node<'_>,
    document_root: Node<'_>,
    source: &[u8],
    work: &mut Work<'_>,
) -> Result<Option<&'static str>, PointerError> {
    if pair.kind() != "pair" {
        return Ok(None);
    }
    let Some(value) = pair.child_by_field_name("value") else {
        return Ok(None);
    };
    if !context(value, document_root, source, work)?.schema {
        return Ok(None);
    }
    let Some(map) = pair
        .parent()
        .and_then(|map| map.parent())
        .filter(|map| map.kind() == "pair")
    else {
        return Ok(None);
    };
    match key(map, source, work)?.as_str() {
        "properties" => Ok(Some("json_schema.field")),
        "$defs" => {
            let object = if value.kind() == "object"
                && let Some(kind) = member(value, "type", source, work)?
                && kind.kind() == "string"
            {
                decode_string(text(kind, source)?, work)? == "object"
            } else {
                false
            };
            Ok(Some(if object {
                "json_schema.object_schema"
            } else {
                "json_schema.named_schema"
            }))
        }
        _ => Ok(None),
    }
}

pub(super) struct CaptureCache<'s, 'c> {
    source: &'s [u8],
    work: Work<'c>,
    maximum: usize,
    initialized: bool,
    valid: bool,
    exhausted: bool,
    full_source: bool,
}

impl<'s, 'c> CaptureCache<'s, 'c> {
    pub(super) fn new(
        source: &'s [u8],
        maximum: usize,
        cancellation: &'c Cancellation,
        full_source: bool,
    ) -> Self {
        // The service can leave its body-node ceiling unbounded. Schema proof
        // still uses the query's existing hard capture bound before scaling work.
        let maximum = maximum.min(super::HARD_MAX_QUERY_CAPTURES);
        let work = maximum.checked_mul(128);
        Self {
            source,
            work: Work::new(work.unwrap_or(0), source.len(), cancellation),
            maximum,
            initialized: false,
            valid: false,
            exhausted: work.is_none(),
            full_source,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn initialize(&mut self, node: Node<'_>) -> Result<(), PointerError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        if !self.full_source {
            return Ok(());
        }
        let document = root(node, &mut self.work)?;
        if document.kind() != "document"
            || document.has_error()
            || document.named_child_count() != 1
        {
            return Ok(());
        }
        let Some(object) = document
            .named_child(0)
            .filter(|node| node.kind() == "object")
        else {
            return Ok(());
        };
        let mut cursor = document.walk();
        let mut visited = 0usize;
        loop {
            self.work.step()?;
            visited = visited
                .checked_add(1)
                .ok_or(PointerError::Barrier("json-schema-pointer-work-limit"))?;
            if visited > self.maximum {
                return Err(PointerError::Barrier("json-schema-pointer-work-limit"));
            }
            if cursor.node().kind() == "comment" || cursor.node().is_missing() {
                return Ok(());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    let Some(dialect) = member(object, "$schema", self.source, &mut self.work)?
                    else {
                        return Ok(());
                    };
                    if decode_string(text(dialect, self.source)?, &mut self.work)? != DIALECT {
                        return Ok(());
                    }
                    // Fragment-only references need no external retrieval. A nested
                    // `$id` is rejected separately before crossing its resource base.
                    if let Some(id) = member(object, "$id", self.source, &mut self.work)? {
                        let value = decode_string(text(id, self.source)?, &mut self.work)?;
                        if !absolute_uri(&value) {
                            return Ok(());
                        }
                    }
                    self.valid = true;
                    return Ok(());
                }
            }
        }
    }
}

/// Select exact native leaves and preserve a separate complete source-address graph.
/// Exhausted proof work closes capture coverage; it never returns a partial certificate.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_, '_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    let result = candidate_checked(node, role, source, cancellation, cache);
    match result {
        Ok(candidate) => Ok(candidate),
        Err(PointerError::Adapter(error)) => Err(error),
        Err(PointerError::Barrier(
            "json-schema-pointer-work-limit" | "json-schema-pointer-string-limit",
        )) => {
            cache.exhausted = true;
            Ok(None)
        }
        Err(PointerError::Barrier(_)) => Ok(None),
    }
}

fn candidate_checked(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_, '_>,
) -> Result<Option<QueryCandidate>, PointerError> {
    cancellation.check()?;
    cache.initialize(node)?;
    let document = root(node, &mut cache.work)?;
    let document_root = document.named_child(0);
    let mut range = node.byte_range();
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module => {
            range = 0..source.len();
            "json_schema.file"
        }
        StructuralRole::Expression if node.kind() == "document" => {
            if cache.valid {
                "json_schema.source_semantics"
            } else {
                "json_schema.unproved_dialect"
            }
        }
        _ if !cache.valid => return Ok(None),
        StructuralRole::Scope => match node.kind() {
            "document" => "json_schema.file",
            "pair" => "json_schema.member",
            _ if node.parent().is_some_and(|parent| parent.kind() == "array") => {
                match node.kind() {
                    "array" => "json_schema.array_element_array",
                    "object" => "json_schema.array_element_object",
                    _ => "json_schema.array_element_scalar",
                }
            }
            "object" => "json_schema.object",
            "array" => "json_schema.array",
            _ => return Ok(None),
        },
        StructuralRole::Signature => "json_schema.member_key",
        StructuralRole::Declaration | StructuralRole::Definition => {
            let pair = if role == StructuralRole::Definition {
                node.parent()
            } else {
                Some(node)
            };
            let Some(pair) = pair else {
                return Ok(None);
            };
            let Some(root) = document_root else {
                return Ok(None);
            };
            let Some(syntax) = declaration(pair, root, source, &mut cache.work)? else {
                return Ok(None);
            };
            if role == StructuralRole::Definition {
                if node.kind() != "string" || pair.child_by_field_name("key") != Some(node) {
                    return Ok(None);
                }
                range = node
                    .start_byte()
                    .checked_add(1)
                    .ok_or(PointerError::Barrier(
                        "json-schema-source-range-unavailable",
                    ))?
                    ..node.end_byte().checked_sub(1).ok_or(PointerError::Barrier(
                        "json-schema-source-range-unavailable",
                    ))?;
                "json_schema.name"
            } else {
                syntax
            }
        }
        StructuralRole::Reference => {
            let Some(pair) = node.parent() else {
                return Ok(None);
            };
            let Some(owner) = pair.parent() else {
                return Ok(None);
            };
            let Some(root) = document_root else {
                return Ok(None);
            };
            if key(pair, source, &mut cache.work)? != "$ref" {
                return Ok(None);
            }
            let owner_context = context(owner, root, source, &mut cache.work)?;
            if !owner_context.schema {
                return Ok(None);
            }
            if owner_context.resource_barrier {
                "json_schema.unsupported_resource_ref"
            } else {
                let resolved = fragment_tokens(text(node, source)?, &mut cache.work)
                    .and_then(|tokens| resolve_native(root, &tokens, source, &mut cache.work));
                match resolved {
                    Ok(target) => {
                        let target_context = context(target, root, source, &mut cache.work)?;
                        let declared = target
                            .parent()
                            .map(|pair| declaration(pair, root, source, &mut cache.work))
                            .transpose()?
                            .flatten();
                        if target_context.resource_barrier {
                            "json_schema.unsupported_resource_ref"
                        } else if target_context.schema && declared.is_some() {
                            "json_schema.local_ref"
                        } else {
                            "json_schema.unresolved_ref"
                        }
                    }
                    Err(PointerError::Barrier(
                        "json-schema-pointer-work-limit" | "json-schema-pointer-string-limit",
                    )) => return Err(PointerError::Barrier("json-schema-pointer-work-limit")),
                    Err(PointerError::Adapter(error)) => return Err(PointerError::Adapter(error)),
                    Err(PointerError::Barrier(_)) => "json_schema.unresolved_ref",
                }
            }
        }
        StructuralRole::Expression => {
            let Some(root) = document_root else {
                return Ok(None);
            };
            if node.kind() == "pair"
                && let Some(owner) = node.parent()
                && context(owner, root, source, &mut cache.work)?.schema
            {
                let keyword = key(node, source, &mut cache.work)?;
                if keyword == "$id" && owner != root {
                    return Ok(Some(QueryCandidate {
                        start: range.start,
                        end: range.end,
                        role,
                        syntax: "json_schema.unsupported_resource",
                        required: false,
                        native_depth: native_depth(node, cancellation)?,
                    }));
                }
                if matches!(
                    keyword.as_str(),
                    "$dynamicRef" | "$anchor" | "$dynamicAnchor"
                ) {
                    return Ok(Some(QueryCandidate {
                        start: range.start,
                        end: range.end,
                        role,
                        syntax: "json_schema.unsupported_dynamic",
                        required: false,
                        native_depth: native_depth(node, cancellation)?,
                    }));
                }
            }
            match declaration(node, root, source, &mut cache.work) {
                Err(PointerError::Adapter(error)) => return Err(PointerError::Adapter(error)),
                Err(PointerError::Barrier(
                    "json-schema-pointer-work-limit" | "json-schema-pointer-string-limit",
                )) => return Err(PointerError::Barrier("json-schema-pointer-work-limit")),
                Err(PointerError::Barrier(_)) => "json_schema.unproved_schema_context",
                Ok(_) => return Ok(None),
            }
        }
        StructuralRole::StringLiteral => "json_schema.string",
        _ => return Ok(None),
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{GrammarFamily, language_for};

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Json))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn original_native_contexts_certify_only_exact_unquoted_definition_leaves_and_reference() {
        let source = include_bytes!("../../tests/fixtures/json_schema/test-schema.json");
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(source, 100_000, &cancellation, true);
        for (begin, end, syntax) in [
            (4852, 4856, "json_schema.object_schema"),
            (5413, 5417, "json_schema.field"),
            (5571, 5576, "json_schema.field"),
            (960, 965, "json_schema.field"),
        ] {
            let key = tree
                .root_node()
                .descendant_for_byte_range(begin - 1, end + 1)
                .unwrap();
            let selected = candidate(
                key,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut cache,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                (selected.start, selected.end, selected.syntax),
                (begin, end, "json_schema.name")
            );
            let declaration = candidate(
                key.parent().unwrap(),
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut cache,
            )
            .unwrap()
            .unwrap();
            assert_eq!(declaration.syntax, syntax);
        }
        let reference = tree
            .root_node()
            .descendant_for_byte_range(1124, 1138)
            .unwrap();
        let selected = candidate(
            reference,
            StructuralRole::Reference,
            source,
            &cancellation,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.syntax, "json_schema.local_ref");
        assert!(selected.start <= 1133 && selected.end >= 1137);
        assert!(
            candidate(
                reference,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut cache
            )
            .unwrap()
            .is_none()
        );
        assert!(!cache.exhausted());
    }

    #[test]
    fn service_ceiling_does_not_preexhaust_native_schema_proof() {
        let source = include_bytes!("../../tests/fixtures/json_schema/test-schema.json");
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(source, usize::MAX, &cancellation, true);
        let key = tree
            .root_node()
            .descendant_for_byte_range(4851, 4857)
            .unwrap();
        let selected = candidate(
            key,
            StructuralRole::Definition,
            source,
            &cancellation,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (selected.start, selected.end, selected.syntax),
            (4852, 4856, "json_schema.name")
        );
        assert!(!cache.exhausted());
    }

    #[test]
    fn incomplete_native_walk_and_proof_work_never_leave_a_schema_certificate() {
        let source = include_bytes!("../../tests/fixtures/json_schema/test-schema.json");
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let key = tree
            .root_node()
            .descendant_for_byte_range(4851, 4857)
            .unwrap();
        for maximum in [0, 8, 32] {
            let mut cache = CaptureCache::new(source, maximum, &cancellation, true);
            assert!(
                candidate(
                    key,
                    StructuralRole::Definition,
                    source,
                    &cancellation,
                    &mut cache,
                )
                .unwrap()
                .is_none()
            );
            assert!(cache.exhausted());
            assert!(!cache.valid);
        }
        let mut cache = CaptureCache::new(source, usize::MAX, &cancellation, true);
        cache.work = Work::new(16, source.len(), &cancellation);
        for _ in 0..2 {
            assert!(
                candidate(
                    key,
                    StructuralRole::Definition,
                    source,
                    &cancellation,
                    &mut cache,
                )
                .unwrap()
                .is_none()
            );
            assert!(cache.exhausted());
            assert!(!cache.valid);
        }
    }

    #[test]
    fn annotation_lookalikes_recovery_and_partitions_cannot_supply_native_schema_proof() {
        for source in [
            r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","examples":[{"properties":{"ghost":{}}}]}"##,
            r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"ghost":{}}} {}"##,
            r##"{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"ghost":{}}}"##,
        ] {
            let bytes = source.as_bytes();
            let tree = parse(bytes);
            let begin = source.find("\"ghost\"").unwrap();
            let key = tree
                .root_node()
                .descendant_for_byte_range(begin, begin + 7)
                .unwrap();
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(bytes, 100_000, &cancellation, true);
            assert!(
                candidate(
                    key,
                    StructuralRole::Definition,
                    bytes,
                    &cancellation,
                    &mut cache
                )
                .unwrap()
                .is_none(),
                "{source}"
            );
        }
        let source = include_bytes!("../../tests/fixtures/json_schema/test-schema.json");
        let tree = parse(source);
        let key = tree
            .root_node()
            .descendant_for_byte_range(4851, 4857)
            .unwrap();
        let cancellation = Cancellation::new();
        let mut partition = CaptureCache::new(source, 100_000, &cancellation, false);
        assert!(
            candidate(
                key,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut partition
            )
            .unwrap()
            .is_none()
        );
        let mut bounded = CaptureCache::new(source, 0, &cancellation, true);
        assert!(
            candidate(
                key,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut bounded
            )
            .unwrap()
            .is_none()
        );
        assert!(bounded.exhausted());
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cancelled = CaptureCache::new(source, 100_000, &cancellation, true);
        assert!(
            candidate(
                key,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut cancelled
            )
            .is_err()
        );
    }
}
