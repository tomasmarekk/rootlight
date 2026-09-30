//! Source-owned VBA conditional-compilation inventories.
//! Canonical envelopes preserve authored branch ranges and producer decisions;
//! structural validation does not authenticate the evaluator's language semantics.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::{FactId, GenerationId, RepositoryId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for a source-written VBA module's conditional-compilation inventory.
pub const VBA_COMPILATION_NAMESPACE: &str = "rootlight.vba_compilation";
/// Canonical encoding version of VBA conditional-compilation inventories.
pub const VBA_COMPILATION_VERSION: &str = "1";
/// Maximum authored conditional branches in one inventory, including empty bodies.
pub const MAX_VBA_COMPILATION_BRANCHES: usize = 1_024;
/// Maximum serialized inventory bytes, below the general extension-payload budget.
///
/// The byte bound independently limits span metadata even at the branch-count
/// bound; no source text or recursively nested wire values are stored.
pub const MAX_VBA_COMPILATION_PAYLOAD_BYTES: usize = 256 * 1024;

/// A producer's proved conditional activity, not runtime statement reachability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VbaCompilationActivity {
    /// The conditional alternative is selected by the producer's environment.
    Active,
    /// The conditional alternative is excluded by the producer's environment.
    Inactive,
    /// Selection could not be established without unsupported or missing inputs.
    Unproven,
}

/// One authored conditional alternative, including an explicitly empty body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VbaCompilationBranch {
    /// Full containing `#If` through `#End If` group, including directives.
    pub group: SourceSpan,
    /// Exact body between directives; nested groups remain inside this range.
    pub region: SourceSpan,
    /// Local alternative selection; ancestor activity is applied during lookup.
    pub activity: VbaCompilationActivity,
}

/// A complete or fail-closed compilation inventory for one source-written scope.
///
/// This is wire data. Consumers must decode its canonical envelope before using
/// decisions. `complete` proves that the producer inventoried every conditional
/// alternative, not that every condition is known or the program is valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VbaCompilation {
    /// Complete source-written module or file range bound by the envelope source.
    pub scope: SourceSpan,
    /// Whether all conditional groups and alternatives in the scope are retained.
    pub complete: bool,
    /// Canonically ordered authored alternatives, with nested ranges preserved.
    #[serde(deserialize_with = "deserialize_branches")]
    pub branches: Vec<VbaCompilationBranch>,
}

fn deserialize_branches<'de, D>(deserializer: D) -> Result<Vec<VbaCompilationBranch>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Branches;

    impl<'de> serde::de::Visitor<'de> for Branches {
        type Value = Vec<VbaCompilationBranch>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded conditional branch inventory")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut branches = Vec::new();
            while let Some(branch) = sequence.next_element()? {
                if branches.len() == MAX_VBA_COMPILATION_BRANCHES {
                    return Err(serde::de::Error::custom("too many conditional branches"));
                }
                branches.push(branch);
            }
            Ok(branches)
        }
    }

    deserializer.deserialize_seq(Branches)
}

impl VbaCompilation {
    /// Returns activity for a nonempty written span within a validated inventory.
    ///
    /// Incomplete inventories, foreign/outside spans, directive ranges and ranges
    /// crossing alternatives are unproven. An inactive enclosing alternative
    /// suppresses even an active or unknown nested alternative. Outside all groups
    /// is active only when the complete inventory explicitly proves it unguarded.
    #[must_use]
    pub fn activity_at(&self, span: SourceSpan) -> VbaCompilationActivity {
        if !self.complete || !contains_nonempty(self.scope, span) {
            return VbaCompilationActivity::Unproven;
        }
        let mut activity = VbaCompilationActivity::Active;
        for branch in &self.branches {
            if contains_nonempty(branch.region, span) {
                match branch.activity {
                    VbaCompilationActivity::Inactive => return VbaCompilationActivity::Inactive,
                    VbaCompilationActivity::Unproven => {
                        activity = VbaCompilationActivity::Unproven;
                    }
                    VbaCompilationActivity::Active => {}
                }
            }
        }
        let mut remaining = self.branches.as_slice();
        while let Some(first) = remaining.first() {
            let count = remaining
                .iter()
                .take_while(|branch| branch.group == first.group)
                .count();
            let (group, tail) = remaining.split_at(count);
            if overlaps(first.group, span)
                && !group
                    .iter()
                    .any(|branch| contains_nonempty(branch.region, span))
            {
                activity = VbaCompilationActivity::Unproven;
            }
            remaining = tail;
        }
        activity
    }
}

/// Invalid bounds, conditional topology, source ownership or canonical encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid vba compilation evidence")]
pub struct VbaCompilationError;

/// Encodes a bounded inventory with canonical order and complete scope ownership.
///
/// # Errors
/// Rejects foreign source ownership, empty scopes/groups, out-of-scope spans,
/// branch bodies touching directives, duplicate/crossing regions, impossible
/// sibling choices and exceeded bounds.
/// Parser provenance and evaluator semantics are checked by their own producers;
/// document validation additionally checks the referenced parser input source.
pub fn new_vba_compilation_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    mut compilation: VbaCompilation,
) -> Result<ExtensionEnvelope, VbaCompilationError> {
    if source.repository() != repository
        || source.generation() != generation
        || source.span() != compilation.scope
    {
        return Err(VbaCompilationError);
    }
    validate_compilation(&compilation)?;
    compilation.branches.sort_by_key(|branch| {
        (
            branch.group.start_byte(),
            branch.group.end_byte(),
            branch.region.start_byte(),
            branch.region.end_byte(),
        )
    });
    let payload = serde_json::to_string(&compilation).map_err(|_| VbaCompilationError)?;
    if payload.len() > MAX_VBA_COMPILATION_PAYLOAD_BYTES {
        return Err(VbaCompilationError);
    }
    let subject = FactRef::File(compilation.scope.file());
    let identity = serde_json::to_vec(&(
        VBA_COMPILATION_NAMESPACE,
        VBA_COMPILATION_VERSION,
        repository,
        generation,
        provenance,
        &source,
        &payload,
        subject,
    ))
    .map_err(|_| VbaCompilationError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.vba_compilation/v1", &identity).id(),
        repository,
        generation,
        namespace: VBA_COMPILATION_NAMESPACE.to_owned(),
        version: VBA_COMPILATION_VERSION.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![subject],
        },
    })
}

/// Decodes an inventory and verifies its complete canonical source-bound envelope.
///
/// # Errors
/// Rejects oversized, altered, stale, foreign, critical, noncanonical or unknown
/// encodings. This establishes structural ownership, not producer authenticity.
pub fn decode_vba_compilation_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<VbaCompilation, VbaCompilationError> {
    if envelope.namespace != VBA_COMPILATION_NAMESPACE
        || envelope.version != VBA_COMPILATION_VERSION
        || envelope.criticality != ExtensionCriticality::Noncritical
        || envelope.payload.len() > MAX_VBA_COMPILATION_PAYLOAD_BYTES
    {
        return Err(VbaCompilationError);
    }
    let compilation: VbaCompilation =
        serde_json::from_str(&envelope.payload).map_err(|_| VbaCompilationError)?;
    let expected = new_vba_compilation_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(VbaCompilationError)?,
        compilation.clone(),
    )?;
    if expected != *envelope {
        return Err(VbaCompilationError);
    }
    Ok(compilation)
}

fn validate_compilation(compilation: &VbaCompilation) -> Result<(), VbaCompilationError> {
    if compilation.scope.start_byte() >= compilation.scope.end_byte()
        || compilation.branches.len() > MAX_VBA_COMPILATION_BRANCHES
    {
        return Err(VbaCompilationError);
    }
    let mut groups: BTreeMap<SourceSpan, Vec<&VbaCompilationBranch>> = BTreeMap::new();
    let mut regions = BTreeSet::new();
    for branch in &compilation.branches {
        if !contains_nonempty(compilation.scope, branch.group)
            || !contains_allow_empty(branch.group, branch.region)
            || branch.region.start_byte() <= branch.group.start_byte()
            || branch.region.end_byte() >= branch.group.end_byte()
            || !regions.insert(branch.region)
        {
            return Err(VbaCompilationError);
        }
        groups.entry(branch.group).or_default().push(branch);
    }
    for branches in groups.values_mut() {
        branches.sort_by_key(|branch| (branch.region.start_byte(), branch.region.end_byte()));
        let mut active = 0;
        let mut unproven = false;
        let mut previous_end = None;
        for branch in branches {
            if previous_end.is_some_and(|end| branch.region.start_byte() < end) {
                return Err(VbaCompilationError);
            }
            previous_end = Some(branch.region.end_byte());
            match branch.activity {
                VbaCompilationActivity::Active => active += 1,
                VbaCompilationActivity::Unproven => unproven = true,
                VbaCompilationActivity::Inactive => {}
            }
        }
        if active > 1 || (active != 0 && unproven) {
            return Err(VbaCompilationError);
        }
    }
    let mut ordered: Vec<_> = groups.iter().collect();
    ordered.sort_by_key(|(span, _)| (span.start_byte(), std::cmp::Reverse(span.end_byte())));
    let mut ancestors: Vec<(&SourceSpan, &Vec<&VbaCompilationBranch>)> = Vec::new();
    for (group, branches) in ordered {
        while ancestors
            .last()
            .is_some_and(|(parent, _)| group.start_byte() >= parent.end_byte())
        {
            ancestors.pop();
        }
        if let Some((parent, alternatives)) = ancestors.last()
            && (!contains_nonempty(**parent, *group)
                || !alternatives
                    .iter()
                    .any(|branch| contains_nonempty(branch.region, *group)))
        {
            return Err(VbaCompilationError);
        }
        ancestors.push((group, branches));
    }
    Ok(())
}

fn contains_nonempty(outer: SourceSpan, inner: SourceSpan) -> bool {
    inner.start_byte() < inner.end_byte() && contains_allow_empty(outer, inner)
}

fn contains_allow_empty(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && outer.start_byte() <= inner.start_byte()
        && inner.end_byte() <= outer.end_byte()
}

fn overlaps(left: SourceSpan, right: SourceSpan) -> bool {
    left.file() == right.file()
        && left.start_byte() < right.end_byte()
        && right.start_byte() < left.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, content_hash};

    fn span(start: u64, end: u64) -> SourceSpan {
        SourceSpan::new(FileId::from_bytes([3; 20]), start, end).unwrap()
    }

    fn compilation() -> VbaCompilation {
        VbaCompilation {
            scope: span(0, 100),
            complete: true,
            branches: vec![
                VbaCompilationBranch {
                    group: span(10, 80),
                    region: span(15, 45),
                    activity: VbaCompilationActivity::Active,
                },
                VbaCompilationBranch {
                    group: span(10, 80),
                    region: span(50, 75),
                    activity: VbaCompilationActivity::Inactive,
                },
                VbaCompilationBranch {
                    group: span(20, 40),
                    region: span(25, 30),
                    activity: VbaCompilationActivity::Inactive,
                },
                VbaCompilationBranch {
                    group: span(20, 40),
                    region: span(35, 38),
                    activity: VbaCompilationActivity::Active,
                },
            ],
        }
    }

    fn source(scope: SourceSpan) -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            scope,
            content_hash(b"source"),
            None,
        )
    }

    fn envelope(value: VbaCompilation) -> Result<ExtensionEnvelope, VbaCompilationError> {
        let source = source(value.scope);
        new_vba_compilation_envelope(
            source.repository(),
            source.generation(),
            FactId::from_bytes([4; 20]),
            source,
            value,
        )
    }

    #[test]
    fn canonical_inventory_roundtrips_without_source_order_dependence() {
        let mut model = compilation();
        let expected = envelope(model.clone()).unwrap();
        model.branches.reverse();
        assert_eq!(envelope(model).unwrap(), expected);
        let decoded = decode_vba_compilation_envelope(&expected).unwrap();
        assert_eq!(envelope(decoded).unwrap(), expected);
        assert_eq!(
            expected.evidence.derivation,
            vec![FactRef::File(compilation().scope.file())]
        );
    }

    #[test]
    fn nested_activity_and_unknown_ancestors_are_fail_closed() {
        let model = decode_vba_compilation_envelope(&envelope(compilation()).unwrap()).unwrap();
        for (start, end, expected) in [
            (0, 5, VbaCompilationActivity::Active),
            (16, 18, VbaCompilationActivity::Active),
            (26, 28, VbaCompilationActivity::Inactive),
            (36, 37, VbaCompilationActivity::Active),
            (51, 55, VbaCompilationActivity::Inactive),
            (81, 90, VbaCompilationActivity::Active),
            (10, 12, VbaCompilationActivity::Unproven),
            (20, 23, VbaCompilationActivity::Unproven),
            (28, 36, VbaCompilationActivity::Unproven),
            (0, 100, VbaCompilationActivity::Unproven),
            (50, 50, VbaCompilationActivity::Unproven),
            (95, 101, VbaCompilationActivity::Unproven),
        ] {
            assert_eq!(model.activity_at(span(start, end)), expected);
        }
        assert_eq!(
            model.activity_at(SourceSpan::new(FileId::from_bytes([9; 20]), 0, 5).unwrap()),
            VbaCompilationActivity::Unproven
        );
        let mut unknown = compilation();
        unknown.branches[0].activity = VbaCompilationActivity::Unproven;
        unknown.branches[1].activity = VbaCompilationActivity::Unproven;
        let unknown = decode_vba_compilation_envelope(&envelope(unknown).unwrap()).unwrap();
        assert_eq!(
            unknown.activity_at(span(36, 37)),
            VbaCompilationActivity::Unproven
        );
        assert_eq!(
            unknown.activity_at(span(26, 28)),
            VbaCompilationActivity::Inactive
        );
        let mut excluded = compilation();
        excluded.branches[0].activity = VbaCompilationActivity::Inactive;
        let excluded = decode_vba_compilation_envelope(&envelope(excluded).unwrap()).unwrap();
        assert_eq!(
            excluded.activity_at(span(36, 37)),
            VbaCompilationActivity::Inactive
        );
    }

    #[test]
    fn incomplete_inventory_never_promotes_absent_or_known_branches() {
        let mut incomplete = compilation();
        incomplete.complete = false;
        let incomplete = decode_vba_compilation_envelope(&envelope(incomplete).unwrap()).unwrap();
        for query in [span(0, 5), span(16, 18), span(26, 28), span(51, 55)] {
            assert_eq!(
                incomplete.activity_at(query),
                VbaCompilationActivity::Unproven
            );
        }
        let mut empty_inventory = compilation();
        empty_inventory.branches.clear();
        assert_eq!(
            empty_inventory.activity_at(span(0, 5)),
            VbaCompilationActivity::Active
        );
        empty_inventory.complete = false;
        assert_eq!(
            empty_inventory.activity_at(span(0, 5)),
            VbaCompilationActivity::Unproven
        );
    }

    #[test]
    fn empty_alternatives_are_retained_but_cannot_select_written_spans() {
        let mut model = compilation();
        model.branches[1].region = span(50, 50);
        let decoded = decode_vba_compilation_envelope(&envelope(model.clone()).unwrap()).unwrap();
        assert!(
            decoded
                .branches
                .iter()
                .any(|branch| branch.region == span(50, 50))
        );
        assert_eq!(
            decoded.activity_at(span(50, 51)),
            VbaCompilationActivity::Unproven
        );
        model.branches.push(model.branches[1].clone());
        assert!(envelope(model).is_err());
    }

    #[test]
    fn branch_bodies_require_opening_and_closing_directive_margins() {
        let mut model = compilation();
        model.branches.truncate(1);
        for region in [
            span(10, 80),
            span(10, 20),
            span(20, 80),
            span(10, 10),
            span(80, 80),
        ] {
            model.branches[0].region = region;
            assert!(envelope(model.clone()).is_err());
        }
        for region in [span(11, 79), span(11, 11), span(79, 79)] {
            model.branches[0].region = region;
            let decoded =
                decode_vba_compilation_envelope(&envelope(model.clone()).unwrap()).unwrap();
            assert_eq!(decoded.branches[0].region, region);
            for directive in [span(10, 11), span(79, 80)] {
                assert_eq!(
                    decoded.activity_at(directive),
                    VbaCompilationActivity::Unproven
                );
            }
            assert_eq!(
                decoded.activity_at(span(11, 12)),
                if region.start_byte() == region.end_byte() {
                    VbaCompilationActivity::Unproven
                } else {
                    VbaCompilationActivity::Active
                }
            );
        }
    }

    #[test]
    fn malformed_conditional_topology_and_choice_sets_are_rejected() {
        for mutate in [
            |value: &mut VbaCompilation| value.scope = span(0, 0),
            |value: &mut VbaCompilation| value.branches[0].group = span(10, 10),
            |value: &mut VbaCompilation| value.branches[0].group = span(10, 101),
            |value: &mut VbaCompilation| value.branches[0].region = span(9, 45),
            |value: &mut VbaCompilation| value.branches[0].region = span(15, 81),
            |value: &mut VbaCompilation| value.branches[1].region = span(40, 75),
            |value: &mut VbaCompilation| {
                value.branches[1].activity = VbaCompilationActivity::Active
            },
            |value: &mut VbaCompilation| {
                value.branches[1].activity = VbaCompilationActivity::Unproven
            },
            |value: &mut VbaCompilation| value.branches[1].region = value.branches[0].region,
            |value: &mut VbaCompilation| value.branches[2].group = span(20, 85),
            |value: &mut VbaCompilation| value.branches[2].group = span(40, 60),
            |value: &mut VbaCompilation| {
                value.branches[2].region =
                    SourceSpan::new(FileId::from_bytes([9; 20]), 25, 30).unwrap();
            },
        ] {
            let mut model = compilation();
            mutate(&mut model);
            assert!(envelope(model).is_err());
        }
    }

    #[test]
    fn canonical_envelope_rejects_identity_source_and_encoding_tampering() {
        let original = envelope(compilation()).unwrap();
        for mutate in [
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.namespace.push('x'),
            |value: &mut ExtensionEnvelope| value.criticality = ExtensionCriticality::Critical,
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| value.provenance = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| value.repository = RepositoryId::from_bytes([9; 16]),
            |value: &mut ExtensionEnvelope| value.generation = GenerationId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| value.evidence.source = None,
            |value: &mut ExtensionEnvelope| value.evidence.source = Some(source(span(0, 99))),
            |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
            |value: &mut ExtensionEnvelope| {
                value
                    .evidence
                    .derivation
                    .push(FactRef::File(FileId::from_bytes([9; 20])))
            },
            |value: &mut ExtensionEnvelope| {
                value.payload = "x".repeat(MAX_VBA_COMPILATION_PAYLOAD_BYTES + 1)
            },
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_vba_compilation_envelope(&changed).is_err());
        }
        let mut reordered = original.clone();
        let mut model = decode_vba_compilation_envelope(&original).unwrap();
        model.branches.reverse();
        reordered.payload = serde_json::to_string(&model).unwrap();
        assert!(decode_vba_compilation_envelope(&reordered).is_err());
        for field in ["unknown", "scope"] {
            let mut wire: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
            if field == "scope" {
                wire[field]["start_byte"] = serde_json::json!(101);
            } else {
                wire[field] = serde_json::json!(true);
            }
            let mut changed = original.clone();
            changed.payload = serde_json::to_string(&wire).unwrap();
            assert!(decode_vba_compilation_envelope(&changed).is_err());
        }
        for changed_source in [
            SourceRef::new(
                RepositoryId::from_bytes([9; 16]),
                source(span(0, 100)).generation(),
                span(0, 100),
                content_hash(b"source"),
                None,
            ),
            SourceRef::new(
                source(span(0, 100)).repository(),
                GenerationId::from_bytes([9; 20]),
                span(0, 100),
                content_hash(b"source"),
                None,
            ),
        ] {
            assert!(
                new_vba_compilation_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    changed_source,
                    compilation()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn nested_inventory_uses_the_branch_budget_without_a_shallow_depth_limit() {
        let count = u64::try_from(MAX_VBA_COMPILATION_BRANCHES).unwrap();
        let end = count * 4 + 4;
        let mut model = VbaCompilation {
            scope: span(0, end),
            complete: true,
            branches: Vec::new(),
        };
        for index in 0..count {
            model.branches.push(VbaCompilationBranch {
                group: span(index * 2, end - index * 2),
                region: span(index * 2 + 1, end - index * 2 - 1),
                activity: VbaCompilationActivity::Active,
            });
        }
        let decoded = decode_vba_compilation_envelope(&envelope(model.clone()).unwrap()).unwrap();
        assert_eq!(decoded.branches.len(), MAX_VBA_COMPILATION_BRANCHES);
        assert_eq!(
            decoded.activity_at(span(count * 2, count * 2 + 1)),
            VbaCompilationActivity::Active
        );
        model.branches.push(VbaCompilationBranch {
            group: span(count * 2, count * 2 + 4),
            region: span(count * 2 + 1, count * 2 + 3),
            activity: VbaCompilationActivity::Active,
        });
        assert!(
            serde_json::from_str::<VbaCompilation>(&serde_json::to_string(&model).unwrap())
                .is_err()
        );
        assert!(envelope(model).is_err());
    }

    fn document() -> crate::NormalizedIrDocument {
        let decoded = crate::decode_ir_document(
            include_bytes!("../../../tests/fixtures/compatibility/ir/1.1/document.json"),
            &crate::IrLimits::default(),
            &crate::ExtensionSupport::default(),
        )
        .unwrap();
        let crate::IrDocument::NormalizedV1_1(mut document) = decoded else {
            panic!("fixture is normalized version 1.1");
        };
        document.extensions.clear();
        document
    }

    fn add_scope(document: &mut crate::NormalizedIrDocument, start: u64, end: u64, complete: bool) {
        let file = &document.files[0];
        let scope = SourceSpan::new(file.id, start, end).unwrap();
        let source = SourceRef::new(
            document.repository,
            document.generation,
            scope,
            file.content_hash,
            None,
        );
        document.extensions.push(
            new_vba_compilation_envelope(
                document.repository,
                document.generation,
                file.provenance,
                source,
                VbaCompilation {
                    scope,
                    complete,
                    branches: Vec::new(),
                },
            )
            .unwrap(),
        );
    }

    #[test]
    fn document_validation_requires_a_matching_containing_parser_input() {
        let mut valid = document();
        add_scope(&mut valid, 2, 24, true);
        crate::validate_ir_document(
            &valid,
            &crate::IrLimits::default(),
            &crate::ExtensionSupport::default(),
        )
        .unwrap();
        for mutate in [
            |value: &mut crate::NormalizedIrDocument| {
                value.provenance[0].producer_kind = crate::ProducerKind::Rule
            },
            |value: &mut crate::NormalizedIrDocument| value.provenance[0].input_sources.clear(),
            |value: &mut crate::NormalizedIrDocument| {
                let source = &value.provenance[0].input_sources[0];
                value.provenance[0].input_sources[0] = SourceRef::new(
                    source.repository(),
                    source.generation(),
                    SourceSpan::new(source.span().file(), 3, 24).unwrap(),
                    source.content_hash(),
                    None,
                );
            },
            |value: &mut crate::NormalizedIrDocument| {
                let source = &value.provenance[0].input_sources[0];
                value.provenance[0].input_sources[0] = SourceRef::new(
                    source.repository(),
                    source.generation(),
                    SourceSpan::new(source.span().file(), 2, 23).unwrap(),
                    source.content_hash(),
                    None,
                );
            },
            |value: &mut crate::NormalizedIrDocument| {
                let source = &value.provenance[0].input_sources[0];
                value.provenance[0].input_sources[0] = SourceRef::new(
                    source.repository(),
                    source.generation(),
                    source.span(),
                    content_hash(b"foreign"),
                    None,
                );
            },
            |value: &mut crate::NormalizedIrDocument| {
                let source = &value.provenance[0].input_sources[0];
                value.provenance[0].input_sources[0] = SourceRef::new(
                    RepositoryId::from_bytes([9; 16]),
                    source.generation(),
                    source.span(),
                    source.content_hash(),
                    None,
                );
            },
            |value: &mut crate::NormalizedIrDocument| {
                let source = &value.provenance[0].input_sources[0];
                value.provenance[0].input_sources[0] = SourceRef::new(
                    source.repository(),
                    GenerationId::from_bytes([9; 20]),
                    source.span(),
                    source.content_hash(),
                    None,
                );
            },
            |value: &mut crate::NormalizedIrDocument| {
                let source = value.extensions[0].evidence.source.as_ref().unwrap();
                let compilation = decode_vba_compilation_envelope(&value.extensions[0]).unwrap();
                let source = SourceRef::new(
                    source.repository(),
                    source.generation(),
                    source.span(),
                    content_hash(b"foreign"),
                    None,
                );
                value.extensions[0] = new_vba_compilation_envelope(
                    value.repository,
                    value.generation,
                    value.extensions[0].provenance,
                    source,
                    compilation,
                )
                .unwrap();
            },
        ] {
            let mut changed = valid.clone();
            mutate(&mut changed);
            assert!(
                crate::validate_ir_document(
                    &changed,
                    &crate::IrLimits::default(),
                    &crate::ExtensionSupport::default()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn document_validation_rejects_conflicting_scopes_and_malformed_skipped_evidence() {
        let mut valid = document();
        add_scope(&mut valid, 2, 10, true);
        add_scope(&mut valid, 10, 24, true);
        crate::canonicalize_ir_document(
            valid.clone(),
            &crate::IrLimits::default(),
            &crate::ExtensionSupport::default(),
        )
        .unwrap();
        for (start, end, complete) in [(2, 10, false), (3, 8, true), (9, 15, true)] {
            let mut changed = valid.clone();
            add_scope(&mut changed, start, end, complete);
            assert!(
                crate::canonicalize_ir_document(
                    changed,
                    &crate::IrLimits::default(),
                    &crate::ExtensionSupport::default()
                )
                .is_err()
            );
        }
        let mut malformed = valid;
        malformed.extensions[0].payload.push(' ');
        let skipped = crate::ExtensionSupport {
            unknown_noncritical: crate::UnknownNoncriticalExtensionPolicy::Skip,
            ..crate::ExtensionSupport::default()
        };
        assert!(
            crate::canonicalize_ir_document(
                malformed.clone(),
                &crate::IrLimits::default(),
                &skipped
            )
            .is_err()
        );
        let bytes = serde_json::to_vec(&malformed.extensions[0]).unwrap();
        assert!(crate::decode_extension_envelope(&bytes, &crate::IrLimits::default()).is_err());
    }
}
