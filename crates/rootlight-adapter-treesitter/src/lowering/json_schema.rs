//! Same-document schema references resolve through retained native data addresses.
//! Decoded keys and every array position are separate from names; only native
//! schema labels and same-frame materialized declarations can prove UsesType.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, SourceSpan};

use crate::json_schema_pointer::{PointerError, Work, decode_string, fragment_tokens};

use super::MaterializedEntity;

#[derive(Clone, Copy, Default)]
enum ValueKind {
    Object,
    Array,
    #[default]
    Unknown,
}

#[derive(Clone, Copy)]
struct Address {
    identity: [u8; 32],
    kind: ValueKind,
}

#[derive(Clone, Copy)]
struct Child {
    address: Option<usize>,
    count: u64,
}

#[derive(Default)]
pub(super) struct Plan {
    addresses: Vec<Address>,
    states: HashMap<u64, usize>,
    children: HashMap<(usize, String), Child>,
    declarations: HashMap<usize, Option<u64>>,
    complete: bool,
    maximum_string_bytes: usize,
    maximum_work: usize,
}

fn copy_string(value: &str) -> Result<String, AdapterError> {
    let mut copy = String::new();
    copy.try_reserve(value.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    copy.push_str(value);
    Ok(copy)
}

fn pointer_error(error: PointerError) -> AdapterError {
    match error {
        PointerError::Adapter(error) => error,
        PointerError::Barrier(reason) => super::provider_failure(reason),
    }
}

fn hash_address(parent: [u8; 32], label: &str, token: &str, ordinal: u64) -> [u8; 32] {
    let mut hash = blake3::Hasher::new_derive_key("rootlight.json-schema-source-address/1");
    hash.update(&parent);
    hash.update(label.as_bytes());
    hash.update(&[0]);
    hash.update(token.as_bytes());
    hash.update(&ordinal.to_be_bytes());
    *hash.finalize().as_bytes()
}

impl Plan {
    /// Bind the required native member/key closure to one parse's source graph.
    /// Incomplete parse coverage may retain declarations, but cannot resolve refs.
    pub(super) fn new<'s>(
        facts: &[&SyntaxFact],
        text: impl Fn(SourceSpan) -> Result<&'s str, AdapterError>,
        complete: bool,
        maximum_string_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let maximum_work = facts
            .len()
            .checked_mul(128)
            .ok_or(SinkError::AccountingOverflow)?;
        let mut work = Work::new(maximum_work, maximum_string_bytes, cancellation);
        let mut keys = HashMap::new();
        keys.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancellation.check()?;
            if fact.kind() != SyntaxFactKind::Signature
                || fact.syntax_kind().as_str() != "json_schema.member_key.signature"
            {
                continue;
            }
            let key = decode_string(text(fact.span())?, &mut work).map_err(pointer_error)?;
            let position = (fact.span().file(), fact.span().start_byte());
            if keys.insert(position, key).is_some() {
                return Err(SinkError::SourceMismatch.into());
            }
        }
        let mut plan = Self {
            complete,
            maximum_string_bytes,
            maximum_work,
            ..Self::default()
        };
        plan.states
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        plan.declarations
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        plan.children
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        plan.addresses
            .try_reserve(
                facts
                    .len()
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?,
            )
            .map_err(|_| SinkError::AllocationFailed)?;
        plan.addresses.push(Address {
            identity: *blake3::hash(b"rootlight.json-schema-root-address/1").as_bytes(),
            kind: ValueKind::Unknown,
        });
        let mut arrays = HashMap::<Option<u64>, u64>::new();
        arrays
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancellation.check()?;
            let parent = fact
                .parent()
                .and_then(|id| plan.states.get(&id))
                .copied()
                .unwrap_or(0);
            let label = fact.syntax_kind().as_str();
            let token = if label == "json_schema.member.scope" {
                let key = keys
                    .get(&(fact.span().file(), fact.span().start_byte()))
                    .ok_or(SinkError::SourceMismatch)?;
                Some(copy_string(key)?)
            } else if matches!(
                label,
                "json_schema.array_element_array.scope"
                    | "json_schema.array_element_object.scope"
                    | "json_schema.array_element_scalar.scope"
            ) {
                let next = arrays.entry(fact.parent()).or_default();
                let value = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                Some(value.to_string())
            } else {
                None
            };
            let address = if let Some(token) = token {
                let slot = plan
                    .children
                    .entry((parent, copy_string(&token)?))
                    .or_insert(Child {
                        address: None,
                        count: 0,
                    });
                let ordinal = slot.count;
                slot.count = slot
                    .count
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?;
                let address = plan.addresses.len();
                let parent_identity = plan
                    .addresses
                    .get(parent)
                    .ok_or(SinkError::SourceMismatch)?
                    .identity;
                plan.addresses.push(Address {
                    identity: hash_address(parent_identity, label, &token, ordinal),
                    kind: ValueKind::Unknown,
                });
                slot.address = (slot.count == 1).then_some(address);
                address
            } else {
                parent
            };
            let kind = match label {
                "json_schema.object.scope" | "json_schema.array_element_object.scope" => {
                    Some(ValueKind::Object)
                }
                "json_schema.array.scope" | "json_schema.array_element_array.scope" => {
                    Some(ValueKind::Array)
                }
                _ => None,
            };
            if let Some(kind) = kind {
                plan.addresses
                    .get_mut(address)
                    .ok_or(SinkError::SourceMismatch)?
                    .kind = kind;
            }
            plan.states.insert(fact.local_id(), address);
            if matches!(
                label,
                "json_schema.object_schema.declaration"
                    | "json_schema.named_schema.declaration"
                    | "json_schema.field.declaration"
            ) {
                plan.declarations
                    .entry(address)
                    .and_modify(|declaration| *declaration = None)
                    .or_insert(Some(fact.local_id()));
            }
        }
        Ok(plan)
    }

    pub(super) fn identity(&self, fact: &SyntaxFact) -> Option<[u8; 32]> {
        self.states
            .get(&fact.local_id())
            .and_then(|address| self.addresses.get(*address))
            .map(|address| address.identity)
    }

    /// Resolve a native-certified reference through exact retained addresses.
    /// Physical source identity and materialized owner/target proof are mandatory.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        raw: &str,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        entities: &HashMap<u64, MaterializedEntity>,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if !self.complete
            || fact.syntax_kind().as_str() != "json_schema.local_ref.reference"
            || occurrence.role != OccurrenceRole::TypeUse
        {
            return Ok(None);
        }
        let Some(owner) = owner.filter(|owner| {
            owner.record.language == "json-schema"
                && matches!(
                    owner.record.kind,
                    EntityKind::Field | EntityKind::Struct | EntityKind::TypeAlias
                )
                && occurrence.enclosing == Some(owner.record.id)
        }) else {
            return Ok(None);
        };
        let mut work = Work::new(self.maximum_work, self.maximum_string_bytes, cancellation);
        let tokens = match fragment_tokens(raw, &mut work) {
            Ok(tokens) => tokens,
            Err(PointerError::Adapter(error)) => return Err(error),
            Err(PointerError::Barrier(_)) => return Ok(None),
        };
        let mut address = 0usize;
        for token in tokens {
            cancellation.check()?;
            if matches!(
                self.addresses.get(address).map(|address| address.kind),
                Some(ValueKind::Array)
            ) && (token.is_empty()
                || (token.starts_with('0') && token != "0")
                || !token.bytes().all(|byte| byte.is_ascii_digit()))
            {
                return Ok(None);
            }
            let Some(child) = self
                .children
                .get(&(address, token))
                .and_then(|child| child.address)
            else {
                return Ok(None);
            };
            address = child;
        }
        let Some(local) = self.declarations.get(&address).copied().flatten() else {
            return Ok(None);
        };
        let Some(target) = entities
            .get(&local)
            .filter(|target| target.record.language == "json-schema")
        else {
            return Ok(None);
        };
        let Some(source) = target.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        for proof in [source, owner_source] {
            if proof.repository() != occurrence.source.repository()
                || proof.generation() != occurrence.source.generation()
                || proof.span().file() != occurrence.source.span().file()
                || proof.content_hash() != occurrence.source.content_hash()
            {
                return Ok(None);
            }
        }
        Ok(Some(target.record.id))
    }
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Struct, "json_schema.object_schema.declaration")
            | (
                EntityKind::TypeAlias,
                "json_schema.named_schema.declaration"
            )
            | (EntityKind::Field, "json_schema.field.declaration")
    )
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "json_schema.local_ref.reference"
            | "json_schema.unresolved_ref.reference"
            | "json_schema.unsupported_resource_ref.reference",
        ) => Some(OccurrenceRole::TypeUse),
        (SyntaxFactKind::StringLiteral, "json_schema.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match fact.syntax_kind().as_str() {
        "json_schema.local_ref.reference" | "json_schema.unresolved_ref.reference" => {
            Some("json-schema-source-pointer-target-unavailable")
        }
        "json_schema.unsupported_resource_ref.reference" => {
            Some("json-schema-nested-resource-base-unavailable")
        }
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match fact.syntax_kind().as_str() {
        "json_schema.source_semantics.expression" => &[(
            FactDomain::Relations,
            "json-schema-external-dynamic-resource-and-validation-semantics-unavailable",
        )],
        "json_schema.unproved_dialect.expression" => &[
            (
                FactDomain::Entities,
                "json-schema-root-dialect-or-document-unproved",
            ),
            (
                FactDomain::Occurrences,
                "json-schema-root-dialect-or-document-unproved",
            ),
            (
                FactDomain::Relations,
                "json-schema-root-dialect-or-document-unproved",
            ),
        ],
        "json_schema.unproved_schema_context.expression" => &[(
            FactDomain::Entities,
            "json-schema-ambiguous-or-unsupported-schema-context",
        )],
        "json_schema.unsupported_resource.expression" => &[(
            FactDomain::Relations,
            "json-schema-nested-resource-base-unavailable",
        )],
        "json_schema.unsupported_dynamic.expression" => &[(
            FactDomain::Relations,
            "json-schema-dynamic-and-anchor-reference-unavailable",
        )],
        _ => &[],
    };
    rows.iter().copied()
}
