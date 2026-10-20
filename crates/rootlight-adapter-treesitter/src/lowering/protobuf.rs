//! Protobuf lowering derives only written, unique same-file type associations.
//! Relative lookup respects nested message shadowing; imports and descriptor
//! validation cannot be inferred from a local source table.

use std::collections::HashSet;

use super::*;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "protobuf.package.declaration")
            | (EntityKind::Struct, "protobuf.message.declaration")
            | (EntityKind::Enum, "protobuf.enum.declaration")
            | (EntityKind::Constant, "protobuf.enum_value.declaration")
            | (EntityKind::Union, "protobuf.oneof.declaration")
            | (
                EntityKind::Field,
                "protobuf.field.declaration"
                    | "protobuf.map_field.declaration"
                    | "protobuf.oneof_field.declaration"
            )
            | (EntityKind::Interface, "protobuf.service.declaration")
            | (EntityKind::Method, "protobuf.rpc.declaration")
    )
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "protobuf.file.module") => Some((
            FactDomain::Relations,
            "protobuf-cross-file-import-descriptor-option-extension-and-wire-validation-unavailable",
        )),
        (SyntaxFactKind::Signature, "protobuf.oneof_terminator.expression") => Some((
            FactDomain::Entities,
            "protobuf-oneof-field-physical-terminator-unavailable",
        )),
        (SyntaxFactKind::Signature, "protobuf.unsupported_construct.expression") => Some((
            FactDomain::Entities,
            "protobuf-written-construct-analysis-unavailable",
        )),
        (SyntaxFactKind::Signature, "protobuf.extension_semantics.expression") => Some((
            FactDomain::Entities,
            "protobuf-extension-owner-and-binding-unavailable",
        )),
        (
            SyntaxFactKind::Signature,
            "protobuf.source_semantics.expression"
            | "protobuf.schema_semantics.expression"
            | "protobuf.field_semantics.expression",
        ) => Some((
            FactDomain::Relations,
            "protobuf-schema-option-number-and-dialect-validation-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "protobuf.type.reference") => {
            Some("protobuf-type-target-unavailable")
        }
        (SyntaxFactKind::Import, "protobuf.written_import.import") => {
            Some("protobuf-import-target-unavailable")
        }
        _ => None,
    }
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "protobuf.type.reference") => Some(OccurrenceRole::TypeUse),
        (SyntaxFactKind::Import, "protobuf.written_import.import") => {
            Some(OccurrenceRole::ImportUse)
        }
        (SyntaxFactKind::Comment, "protobuf.comment.comment") => {
            Some(OccurrenceRole::Documentation)
        }
        (SyntaxFactKind::StringLiteral, "protobuf.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

struct Context {
    parent: Option<SymbolId>,
    namespace: Option<String>,
}

/// Indexes source-owned types without treating imports as resolved dependencies.
pub(super) struct Bindings {
    targets: HashMap<String, Option<SymbolId>>,
    prefixes: HashSet<String>,
    contexts: HashMap<SymbolId, Context>,
    package: String,
    available: bool,
    maximum_name_bytes: usize,
}

fn owned(text: &str) -> Result<String, AdapterError> {
    let mut result = String::new();
    result
        .try_reserve_exact(text.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    result.push_str(text);
    Ok(result)
}

fn joined(prefix: &str, suffix: &str, maximum: usize) -> Result<String, AdapterError> {
    let length = prefix
        .len()
        .checked_add(suffix.len())
        .and_then(|length| length.checked_add(usize::from(!prefix.is_empty())))
        .ok_or(SinkError::AccountingOverflow)?;
    require_resource_limit(ResourceKind::StringBytes, length, maximum)?;
    let mut result = String::new();
    result
        .try_reserve_exact(length)
        .map_err(|_| SinkError::AllocationFailed)?;
    result.push_str(prefix);
    if !prefix.is_empty() {
        result.push('.');
    }
    result.push_str(suffix);
    Ok(result)
}

// Display-qualified names include the physical file module. Protobuf type
// spelling instead contains only written message/enum owners and the package.
fn type_namespace<'a>(
    entity: &'a MaterializedEntity,
    owners: &HashMap<SymbolId, &'a MaterializedEntity>,
    package: &str,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<String, AdapterError> {
    let mut names = Vec::new();
    let mut current = Some(entity);
    let mut visited = 0_usize;
    let mut length = package.len();
    require_resource_limit(ResourceKind::StringBytes, length, maximum)?;
    while let Some(owner) = current {
        cancellation.check()?;
        visited = visited
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        if visited > owners.len() {
            return Err(provider_failure("protobuf-type-owner-cycle"));
        }
        if owner.record.language != "protobuf" {
            return Err(provider_failure("protobuf-type-owner-language"));
        }
        if matches!(owner.record.kind, EntityKind::Struct | EntityKind::Enum) {
            let name = owner.record.canonical_name.as_str();
            length = length
                .checked_add(name.len())
                .and_then(|value| value.checked_add(usize::from(length != 0)))
                .ok_or(SinkError::AccountingOverflow)?;
            require_resource_limit(ResourceKind::StringBytes, length, maximum)?;
            names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            names.push(name);
        }
        current = match owner.direct_parent {
            ContainerRef::Entity(parent) => Some(
                owners
                    .get(&parent)
                    .copied()
                    .ok_or_else(|| provider_failure("protobuf-type-owner-missing"))?,
            ),
            ContainerRef::File(_) | ContainerRef::Repository(_) => None,
        };
    }
    let mut result = owned(package)?;
    for name in names.into_iter().rev() {
        cancellation.check()?;
        result = joined(&result, name, maximum)?;
    }
    Ok(result)
}

impl Bindings {
    /// Builds a bounded index only after the caller proves complete standalone facts.
    pub(super) fn new(
        materialized: &HashMap<u64, MaterializedEntity>,
        facts: &[SyntaxFact],
        source: &[u8],
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut package = None;
        let mut available = true;
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() != "protobuf.package_name.definition" {
                continue;
            }
            let start = usize::try_from(fact.span().start_byte())
                .map_err(|_| SinkError::AccountingOverflow)?;
            let end = usize::try_from(fact.span().end_byte())
                .map_err(|_| SinkError::AccountingOverflow)?;
            let text = source
                .get(start..end)
                .and_then(|raw| std::str::from_utf8(raw).ok())
                .ok_or_else(|| provider_failure("protobuf-package-source"))?;
            let name = rootlight_adapter_sdk::structural_captured_name_for_fact(
                "protobuf",
                fact,
                text,
                maximum_name_bytes,
            )
            .ok_or_else(|| provider_failure("protobuf-package-name"))?;
            if package.replace(owned(&name)?).is_some() {
                available = false;
            }
        }
        let mut result = Self {
            targets: HashMap::new(),
            prefixes: HashSet::new(),
            contexts: HashMap::new(),
            package: package.unwrap_or_default(),
            available,
            maximum_name_bytes,
        };
        result
            .targets
            .try_reserve(materialized.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .contexts
            .try_reserve(materialized.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut owners = HashMap::new();
        owners
            .try_reserve(materialized.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in materialized
            .values()
            .filter(|entity| entity.record.language == "protobuf")
        {
            cancellation.check()?;
            owners.entry(entity.record.id).or_insert(entity);
        }
        for entity in materialized
            .values()
            .filter(|entity| entity.record.language == "protobuf")
        {
            cancellation.check()?;
            let is_type = matches!(entity.record.kind, EntityKind::Struct | EntityKind::Enum);
            let namespace = if is_type {
                let path = type_namespace(
                    entity,
                    &owners,
                    &result.package,
                    maximum_name_bytes,
                    cancellation,
                )?;
                result
                    .targets
                    .entry(owned(&path)?)
                    .and_modify(|target| *target = None)
                    .or_insert(Some(entity.record.id));
                let mut end = 0_usize;
                for segment in path.split('.') {
                    cancellation.check()?;
                    end = end
                        .checked_add(segment.len())
                        .ok_or(SinkError::AccountingOverflow)?;
                    let prefix = path
                        .get(..end)
                        .ok_or_else(|| provider_failure("protobuf-type-prefix"))?;
                    result
                        .prefixes
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    result.prefixes.insert(owned(prefix)?);
                    end = end.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                }
                Some(path)
            } else {
                None
            };
            let parent = match entity.direct_parent {
                ContainerRef::Entity(parent) => Some(parent),
                _ => None,
            };
            result
                .contexts
                .insert(entity.record.id, Context { parent, namespace });
        }
        Ok(result)
    }

    fn probe(&self, scope: &str, written: &str) -> Result<Option<Option<SymbolId>>, AdapterError> {
        let first = written.split('.').next().unwrap_or(written);
        let first_path = joined(scope, first, self.maximum_name_bytes)?;
        if !self.prefixes.contains(&first_path) {
            return Ok(None);
        }
        // Once the first component binds in an inner scope, a missing tail must
        // not fall through to an unrelated outer type with the same first name.
        let whole = joined(scope, written, self.maximum_name_bytes)?;
        Ok(Some(self.targets.get(&whole).copied().flatten()))
    }

    /// Resolves only one unique written type in this file's lexical/package scopes.
    pub(super) fn resolve(
        &self,
        written: &str,
        enclosing: Option<SymbolId>,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if !self.available {
            return Ok(None);
        }
        if let Some(absolute) = written.strip_prefix('.') {
            return Ok(self.targets.get(absolute).copied().flatten());
        }
        let mut current = enclosing;
        let mut visited = 0_usize;
        while let Some(symbol) = current {
            cancellation.check()?;
            visited = visited
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            if visited > self.contexts.len() {
                return Err(provider_failure("protobuf-type-owner-cycle"));
            }
            let Some(context) = self.contexts.get(&symbol) else {
                break;
            };
            if let Some(namespace) = &context.namespace
                && let Some(target) = self.probe(namespace, written)?
            {
                return Ok(target);
            }
            current = context.parent;
        }
        let mut package_scope = self.package.as_str();
        loop {
            cancellation.check()?;
            if let Some(target) = self.probe(package_scope, written)? {
                return Ok(target);
            }
            if package_scope.is_empty() {
                return Ok(None);
            }
            package_scope = package_scope
                .rsplit_once('.')
                .map_or("", |(parent, _)| parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> Bindings {
        let outer = SymbolId::from_bytes([1; 20]);
        let nested = SymbolId::from_bytes([2; 20]);
        let shadow = SymbolId::from_bytes([3; 20]);
        Bindings {
            targets: [
                ("api.Outer".into(), Some(outer)),
                ("api.Outer.Nested".into(), Some(nested)),
                ("api.Nested.Child".into(), Some(shadow)),
            ]
            .into(),
            prefixes: [
                "api",
                "api.Outer",
                "api.Outer.Nested",
                "api.Nested",
                "api.Nested.Child",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            contexts: [(
                outer,
                Context {
                    parent: None,
                    namespace: Some("api.Outer".into()),
                },
            )]
            .into(),
            package: "api".into(),
            available: true,
            maximum_name_bytes: 256,
        }
    }

    #[test]
    fn nested_local_and_absolute_types_have_no_outer_tail_fallback() {
        let index = index();
        let cancellation = Cancellation::new();
        let outer = Some(SymbolId::from_bytes([1; 20]));
        assert_eq!(
            index.resolve("Nested", outer, &cancellation).unwrap(),
            Some(SymbolId::from_bytes([2; 20]))
        );
        assert_eq!(
            index
                .resolve(".api.Outer.Nested", None, &cancellation)
                .unwrap(),
            Some(SymbolId::from_bytes([2; 20]))
        );
        assert_eq!(
            index.resolve("Nested.Child", outer, &cancellation).unwrap(),
            None
        );
        assert_eq!(
            index.resolve("Missing", outer, &cancellation).unwrap(),
            None
        );
    }

    #[test]
    fn duplicate_targets_multiple_packages_and_case_mismatches_remain_unresolved() {
        let mut index = index();
        let cancellation = Cancellation::new();
        assert_eq!(index.resolve("outer", None, &cancellation).unwrap(), None);
        index.targets.insert("api.Outer".into(), None);
        assert_eq!(index.resolve("Outer", None, &cancellation).unwrap(), None);
        index.available = false;
        assert_eq!(
            index
                .resolve(".api.Outer.Nested", None, &cancellation)
                .unwrap(),
            None
        );
    }

    #[test]
    fn relative_names_visit_each_written_package_scope() {
        let mut index = index();
        index.package = "root.api".into();
        index.targets = index
            .targets
            .into_iter()
            .map(|(name, target)| (format!("root.{name}"), target))
            .collect();
        index.prefixes = index
            .prefixes
            .into_iter()
            .map(|name| format!("root.{name}"))
            .collect();
        index.prefixes.insert("root".into());
        assert_eq!(
            index
                .resolve("api.Outer.Nested", None, &Cancellation::new())
                .unwrap(),
            Some(SymbolId::from_bytes([2; 20]))
        );
    }
}
