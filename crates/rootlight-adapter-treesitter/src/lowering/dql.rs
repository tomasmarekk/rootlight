//! Admit a source-owned native query atomically, without software declarations.

use std::{io, mem::size_of};

use super::*;
use rootlight_ir::{
    DQL_QUERY_NAMESPACE, DQL_QUERY_VERSION, DqlCallKind, DqlLiteralKind, DqlNode, DqlNodeKind,
    DqlQuery, MAX_DQL_QUERY_NODES, MAX_DQL_QUERY_PAYLOAD_BYTES, new_dql_query_envelope,
};

pub(super) struct Gap {
    pub(super) span: SourceSpan,
    pub(super) detail: &'static str,
}

#[derive(Default)]
pub(super) struct Plan {
    query: Option<DqlQuery>,
    payload_bytes: usize,
    pub(super) gap: Option<Gap>,
    pub(super) modeled_peak: usize,
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str().starts_with("dql.")
        && fact.syntax_kind().as_str().ends_with(".expression")
}

pub(super) fn transport(fact: &SyntaxFact, language: &str) -> bool {
    language == "dql"
        && fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "dql.file.module"
}

fn role(label: &str, span: SourceSpan) -> Option<DqlNodeKind> {
    Some(match label {
        "dql.operation.expression" => DqlNodeKind::Operation,
        "dql.parameter.expression" => DqlNodeKind::Parameter {
            value_type: span,
            required: false,
        },
        "dql.binding.expression" => DqlNodeKind::Binding,
        "dql.parameter_use.expression" => DqlNodeKind::ParameterReference,
        "dql.variable_use.expression" => DqlNodeKind::VariableReference,
        "dql.value.expression" => DqlNodeKind::Call {
            class: DqlCallKind::Value,
        },
        "dql.root.expression" => DqlNodeKind::Root { aggregation: false },
        "dql.selection.expression" => DqlNodeKind::Selection,
        "dql.predicate.expression" => DqlNodeKind::Predicate { reverse: false },
        "dql.generator.expression" => DqlNodeKind::Call {
            class: DqlCallKind::Generator,
        },
        "dql.filter_call.expression" => DqlNodeKind::Call {
            class: DqlCallKind::Filter,
        },
        "dql.nested_call.expression" => DqlNodeKind::Call {
            class: DqlCallKind::Nested,
        },
        "dql.count.expression" => DqlNodeKind::Call {
            class: DqlCallKind::Count,
        },
        "dql.filter.expression" => DqlNodeKind::Filter,
        "dql.language.expression" => DqlNodeKind::Language,
        "dql.string.expression" => DqlNodeKind::Literal {
            class: DqlLiteralKind::String,
        },
        "dql.number.expression" => DqlNodeKind::Literal {
            class: DqlLiteralKind::Number,
        },
        "dql.boolean.expression" => DqlNodeKind::Literal {
            class: DqlLiteralKind::Boolean,
        },
        "dql.regex.expression" => DqlNodeKind::Literal {
            class: DqlLiteralKind::Regex,
        },
        "dql.list.expression" => DqlNodeKind::List,
        _ => return None,
    })
}

fn name_role(role: DqlNodeKind) -> Option<&'static str> {
    match role {
        DqlNodeKind::Operation => Some("dql.operation_name.expression"),
        DqlNodeKind::Parameter { .. } => Some("dql.parameter_name.expression"),
        DqlNodeKind::Binding => Some("dql.binding_name.expression"),
        DqlNodeKind::Root { .. } => Some("dql.root_name.expression"),
        DqlNodeKind::Predicate { .. } => Some("dql.predicate_name.expression"),
        DqlNodeKind::Call { .. } => Some("dql.call_name.expression"),
        _ => None,
    }
}

fn rank(role: DqlNodeKind) -> u8 {
    match role {
        DqlNodeKind::Operation => 0,
        DqlNodeKind::Root { .. } => 1,
        DqlNodeKind::Selection => 2,
        DqlNodeKind::Call { .. } => 3,
        DqlNodeKind::Filter => 4,
        DqlNodeKind::Predicate { .. } => 5,
        _ => 6,
    }
}

fn contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && outer.start_byte() <= inner.start_byte()
        && inner.end_byte() <= outer.end_byte()
}

fn text(source: &str, span: SourceSpan) -> Option<&str> {
    source.get(usize::try_from(span.start_byte()).ok()?..usize::try_from(span.end_byte()).ok()?)
}

struct PayloadBytes(usize);

impl io::Write for PayloadBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("DQL payload size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Plan {
    pub(super) fn len(&self) -> usize {
        usize::from(self.query.is_some())
    }

    fn refuse(span: SourceSpan, detail: &'static str) -> Self {
        Self {
            gap: Some(Gap { span, detail }),
            ..Self::default()
        }
    }

    pub(super) fn new(
        request: &AnalysisRequest<'_>,
        output: &ParseOutput,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        if request.language().as_str() != "dql" {
            return Ok(Self::default());
        }
        let source = request.source().source_ref();
        let bytes = request.source().bytes();
        let Ok(source_text) = std::str::from_utf8(bytes) else {
            return Ok(Self::refuse(
                source.span(),
                "dql-whole-physical-utf8-source-required",
            ));
        };
        if request.encoding().as_str() != "utf-8"
            || !request.included_ranges().is_empty()
            || source.span().start_byte() != 0
            || usize::try_from(source.span().end_byte()).ok() != Some(bytes.len())
        {
            return Ok(Self::refuse(
                source.span(),
                "dql-whole-physical-utf8-source-required",
            ));
        }
        let count = output.facts().iter().filter(|fact| required(fact)).count();
        let count_nodes = output
            .facts()
            .iter()
            .filter(|fact| {
                required(fact) && role(fact.syntax_kind().as_str(), fact.span()).is_some()
            })
            .count();
        require_resource_limit(ResourceKind::Records, count_nodes, MAX_DQL_QUERY_NODES)?;
        require_resource_limit(
            ResourceKind::ExtensionBytes,
            bytes.len(),
            MAX_DQL_QUERY_PAYLOAD_BYTES,
        )?;
        // Bound retained nodes, metadata, opaque-token scanning and codec temporaries
        // before allocating. The physical source bounds the opaque scanner's ranges.
        let base = output
            .report()
            .resources()
            .reported_memory_bytes()
            .unwrap_or(0);
        let live = checked_add(
            bytes
                .len()
                .checked_mul(32)
                .ok_or(SinkError::AccountingOverflow)?,
            count
                .checked_mul(
                    size_of::<DqlNode>() * 3 + size_of::<&SyntaxFact>() + size_of::<usize>(),
                )
                .ok_or(SinkError::AccountingOverflow)?,
        )?;
        let mut peak = checked_add(base, live)?;
        require_resource_limit(
            ResourceKind::ReportedMemoryBytes,
            peak,
            request.limits().max_reported_memory_bytes(),
        )?;
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(count)
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(count_nodes)
            .map_err(|_| SinkError::AllocationFailed)?;
        for (index, fact) in output.facts().iter().enumerate() {
            check_periodically(index, cancellation)?;
            if !required(fact) {
                continue;
            }
            if !contains(source.span(), fact.span()) || text(source_text, fact.span()).is_none() {
                return Ok(Self::refuse(
                    source.span(),
                    "dql-native-role-evidence-unavailable",
                ));
            }
            if let Some(mut kind) = role(fact.syntax_kind().as_str(), fact.span()) {
                if matches!(kind, DqlNodeKind::Predicate { .. }) {
                    kind = DqlNodeKind::Predicate {
                        reverse: text(source_text, fact.span()).is_some_and(|s| s.starts_with('~')),
                    };
                }
                if let DqlNodeKind::Parameter { value_type, .. } = kind {
                    kind = DqlNodeKind::Parameter {
                        value_type,
                        required: text(source_text, fact.span())
                            .is_some_and(|value| value.trim_end().ends_with('!')),
                    };
                }
                nodes.push(DqlNode {
                    span: fact.span(),
                    parent: None,
                    role: kind,
                    name: None,
                });
            } else if !matches!(
                fact.syntax_kind().as_str(),
                "dql.operation_name.expression"
                    | "dql.root_name.expression"
                    | "dql.predicate_name.expression"
                    | "dql.call_name.expression"
                    | "dql.parameter_name.expression"
                    | "dql.parameter_type.expression"
                    | "dql.binding_name.expression"
            ) {
                return Ok(Self::refuse(fact.span(), "dql-native-construct-unmodeled"));
            }
            facts.push(fact);
        }
        sort_owned_cancellable_by(&mut nodes, cancellation, |a, b| {
            (
                a.span.start_byte(),
                std::cmp::Reverse(a.span.end_byte()),
                rank(a.role),
            )
                .cmp(&(
                    b.span.start_byte(),
                    std::cmp::Reverse(b.span.end_byte()),
                    rank(b.role),
                ))
        })?;
        let mut stack: Vec<usize> = Vec::new();
        stack
            .try_reserve_exact(nodes.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for index in 0..nodes.len() {
            check_periodically(index, cancellation)?;
            if index > 0
                && nodes[index].span == nodes[index - 1].span
                && nodes[index].role == nodes[index - 1].role
            {
                return Ok(Self::refuse(
                    nodes[index].span,
                    "dql-duplicate-native-role-evidence",
                ));
            }
            while stack
                .last()
                .is_some_and(|parent| !contains(nodes[*parent].span, nodes[index].span))
            {
                stack.pop();
            }
            nodes[index].parent = stack
                .last()
                .map(|parent| u32::try_from(*parent))
                .transpose()
                .map_err(|_| SinkError::AccountingOverflow)?;
            stack.push(index);
        }
        for (index, fact) in facts.into_iter().enumerate() {
            check_periodically(index, cancellation)?;
            if role(fact.syntax_kind().as_str(), fact.span()).is_some() {
                continue;
            }
            let type_annotation = fact.syntax_kind().as_str() == "dql.parameter_type.expression";
            let owner = nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    (name_role(node.role) == Some(fact.syntax_kind().as_str())
                        || (type_annotation && matches!(node.role, DqlNodeKind::Parameter { .. })))
                        && contains(node.span, fact.span())
                })
                .min_by_key(|(_, node)| node.span.end_byte() - node.span.start_byte())
                .map(|(index, _)| index);
            let Some(owner) = owner else {
                return Ok(Self::refuse(
                    fact.span(),
                    "dql-native-name-ownership-unavailable",
                ));
            };
            let node = &mut nodes[owner];
            if type_annotation {
                let DqlNodeKind::Parameter {
                    value_type,
                    required,
                } = node.role
                else {
                    return Err(provider_failure("dql-parameter-type-owner"));
                };
                if value_type != node.span {
                    return Ok(Self::refuse(
                        node.span,
                        "dql-native-type-ownership-unavailable",
                    ));
                }
                node.role = DqlNodeKind::Parameter {
                    value_type: fact.span(),
                    required,
                };
                continue;
            }
            let offset = u64::from(matches!(
                node.role,
                DqlNodeKind::Predicate { reverse: true }
            ));
            if node.name.replace(fact.span()).is_some()
                || (node.role != DqlNodeKind::Operation
                    && fact.span().start_byte() != node.span.start_byte() + offset)
            {
                return Ok(Self::refuse(
                    node.span,
                    "dql-native-name-ownership-unavailable",
                ));
            }
        }
        if !nodes
            .iter()
            .any(|node| matches!(node.role, DqlNodeKind::Root { .. }))
        {
            return Ok(Self::refuse(
                source.span(),
                "dql-native-root-inventory-unavailable",
            ));
        }
        for node in &nodes {
            if let DqlNodeKind::Parameter { value_type, .. } = node.role {
                let colon = node.name.and_then(|name| {
                    source_text.get(
                        usize::try_from(name.end_byte()).ok()?
                            ..usize::try_from(value_type.start_byte()).ok()?,
                    )
                });
                if value_type == node.span || !colon.is_some_and(|value| value.trim() == ":") {
                    return Ok(Self::refuse(
                        node.span,
                        "dql-native-type-ownership-unavailable",
                    ));
                }
            }
        }
        let Ok(query) = DqlQuery::from_evidence(source.clone(), source_text, nodes) else {
            return Ok(Self::refuse(
                source.span(),
                "dql-native-query-evidence-unavailable",
            ));
        };
        let mut payload = PayloadBytes(0);
        serde_json::to_writer(&mut payload, &query)
            .map_err(|_| provider_failure("dql-payload-accounting"))?;
        require_resource_limit(
            ResourceKind::ExtensionBytes,
            payload.0,
            request
                .limits()
                .ir()
                .max_extension_payload_bytes
                .min(MAX_DQL_QUERY_PAYLOAD_BYTES),
        )?;
        require_resource_limit(
            ResourceKind::ExtensionBytes,
            payload.0,
            request.limits().ir().max_total_extension_bytes,
        )?;
        peak = checked_add(
            peak,
            payload
                .0
                .checked_mul(8)
                .ok_or(SinkError::AccountingOverflow)?,
        )?;
        require_resource_limit(
            ResourceKind::ReportedMemoryBytes,
            peak,
            request.limits().max_reported_memory_bytes(),
        )?;
        Ok(Self {
            query: Some(query),
            payload_bytes: payload.0,
            modeled_peak: peak,
            gap: None,
        })
    }

    pub(super) fn account_strings(
        &self,
        total: &mut usize,
        limits: &IrLimits,
    ) -> Result<(), AdapterError> {
        if self.query.is_some() {
            for length in [DQL_QUERY_NAMESPACE.len(), DQL_QUERY_VERSION.len()] {
                account_string(total, length, limits)?;
            }
        }
        if let Some(gap) = &self.gap {
            account_string(total, gap.detail.len(), limits)?;
        }
        Ok(())
    }

    pub(super) fn envelope(
        &mut self,
        provenance: FactId,
    ) -> Result<Option<ExtensionEnvelope>, AdapterError> {
        let Some(query) = self.query.take() else {
            return Ok(None);
        };
        let envelope = new_dql_query_envelope(provenance, &query)
            .map_err(|_| provider_failure("dql-query-envelope"))?;
        if envelope.payload.len() != self.payload_bytes {
            return Err(provider_failure("dql-payload-accounting"));
        }
        Ok(Some(envelope))
    }
}
