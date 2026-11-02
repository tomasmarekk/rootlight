//! Typed native DQL query ownership over complete, immutable UTF-8 source.
//! Result keys, predicates and builtins are query data; local bindings are
//! distinct from unresolved database names and query execution.

use std::io::{self, Write};

use rootlight_ids::{FactId, GenerationId, content_hash, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for source-backed native DQL query structure.
pub const DQL_QUERY_NAMESPACE: &str = "rootlight.dql_query";
/// Canonical native DQL query payload revision.
pub const DQL_QUERY_VERSION: &str = "1";
/// Maximum canonical payload size, including JSON escape expansion.
pub const MAX_DQL_QUERY_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Maximum native semantic nodes in one query document.
pub const MAX_DQL_QUERY_NODES: usize = 4096;

/// Native function ownership, without asserting a callable software symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DqlCallKind {
    /// Root generator introduced by the native `func:` argument.
    Generator,
    /// A function leaf inside a boolean filter.
    Filter,
    /// A native `count`, `val`, `len` or `uid` argument call.
    Nested,
    /// A selected count expression.
    Count,
    /// A selected value-variable expression.
    Value,
    /// A selected aggregation expression.
    Aggregate,
}

/// A native boolean operator, independently of source capitalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DqlBooleanOperator {
    /// Conjunction.
    And,
    /// Disjunction.
    Or,
    /// Unary negation.
    Not,
}

/// Native lexical value classification, without database type inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DqlLiteralKind {
    /// A DQL string using the native escape contract.
    String,
    /// A written number retained without floating-point conversion.
    Number,
    /// A written boolean.
    Boolean,
    /// A native regular-expression spelling; no regex engine is executed.
    Regex,
}

/// Native query roles certified by the parser, rather than generic software kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DqlNodeKind {
    /// The one named or anonymous operation in the physical document.
    Operation,
    /// A result root or a generator-free aggregation root.
    Root {
        /// Whether this is a native empty-argument aggregation root.
        aggregation: bool,
    },
    /// A selected result field; an alias is a separate child role.
    Selection,
    /// An unresolved database predicate, with explicit reverse-edge meaning.
    Predicate {
        /// Whether a native `~` prefix was written.
        reverse: bool,
    },
    /// A native builtin or filter call, with no invented software target.
    Call {
        /// The native call context.
        class: DqlCallKind,
    },
    /// A native filter directive, including the valid empty filter.
    Filter,
    /// A boolean expression whose name span selects the operator spelling.
    Boolean {
        /// The native operator and corresponding arity.
        operator: DqlBooleanOperator,
    },
    /// A parenthesized filter expression.
    Group,
    /// A native predicate language suffix, including `@.` and `@*`.
    Language,
    /// A declared external parameter; the name span includes its `$` prefix.
    Parameter {
        /// Source-owned native parameter type spelling.
        value_type: SourceSpan,
        /// Whether an authored `!` makes the parameter required.
        required: bool,
    },
    /// A graph/value variable introduced by native `name as` syntax.
    Binding,
    /// An external parameter use, distinct from a quoted dollar string.
    ParameterReference,
    /// A graph/value variable use in a supported native variable call.
    VariableReference,
    /// A source-authored output alias, never a schema declaration.
    Alias,
    /// A native scalar/regex argument.
    Literal {
        /// The native lexical value class.
        class: DqlLiteralKind,
    },
    /// A native argument list value, preserving ordered child values.
    List,
}

/// Producer-attested native role evidence; a query constructor validates ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DqlNode {
    /// Complete physical range of this native construct.
    pub span: SourceSpan,
    /// Earlier parent index in the bounded native node tree; only operation is root.
    pub parent: Option<u32>,
    /// Native meaning of this construct.
    pub role: DqlNodeKind,
    /// Exact name/operator range when the native role has an authored name.
    pub name: Option<SourceSpan>,
}

/// A local variable reference and its unique source-owned declaration, if available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DqlReference {
    /// Index of a parameter or graph/value variable reference node.
    pub node: u32,
    /// Same-operation declaration; absent means genuinely unresolved.
    pub declaration: Option<u32>,
}

/// A bounded native query graph with exact source and derived local binding edges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DqlQuery {
    source: SourceRef,
    text: String,
    nodes: Vec<DqlNode>,
    references: Vec<DqlReference>,
}

/// Invalid ownership, native role evidence or canonical encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid native dql query evidence")]
pub struct DqlQueryError;

impl DqlQuery {
    /// Validates a parser-attested native query over the complete physical file.
    ///
    /// This authenticates ownership and native role relationships, not parser
    /// trust, database schema targets, execution or all DQL semantic constraints.
    ///
    /// # Errors
    /// Rejects foreign/empty/non-UTF-8-boundary spans, invalid parents, names,
    /// file hashes, node limits, or inconsistent local binding evidence.
    pub fn from_evidence(
        source: SourceRef,
        text: &str,
        nodes: Vec<DqlNode>,
    ) -> Result<Self, DqlQueryError> {
        if text.len() > MAX_DQL_QUERY_PAYLOAD_BYTES
            || nodes.is_empty()
            || nodes.len() > MAX_DQL_QUERY_NODES
            || source.span().start_byte() != 0
            || usize::try_from(source.span().end_byte()).ok() != Some(text.len())
            || source.content_hash() != content_hash(text.as_bytes())
        {
            return Err(DqlQueryError);
        }
        let opaque = opaque_regions(text)?;
        for (index, node) in nodes.iter().enumerate() {
            let raw = slice(&source, text, node.span)?;
            if !opaque_ownership(node, &opaque)? {
                return Err(DqlQueryError);
            }
            let parent = match node.parent {
                Some(parent) => {
                    let parent = usize::try_from(parent).map_err(|_| DqlQueryError)?;
                    if parent >= index {
                        return Err(DqlQueryError);
                    }
                    let owner = nodes.get(parent).ok_or(DqlQueryError)?;
                    if !contains(owner.span, node.span) {
                        return Err(DqlQueryError);
                    }
                    Some(owner)
                }
                None if index == 0 && node.role == DqlNodeKind::Operation => None,
                None => return Err(DqlQueryError),
            };
            if index > 0 && matches!(node.role, DqlNodeKind::Operation) {
                return Err(DqlQueryError);
            }
            if let Some(name) = node.name {
                let name_text = slice(&source, text, name)?;
                if !contains(node.span, name) || !valid_name(node.role, name_text) {
                    return Err(DqlQueryError);
                }
            } else if requires_name(node.role) {
                return Err(DqlQueryError);
            }
            if !allowed_parent(node.role, parent.map(|node| node.role)) || raw.is_empty() {
                return Err(DqlQueryError);
            }
            if let DqlNodeKind::Parameter { value_type, .. } = node.role
                && (!contains(node.span, value_type)
                    || !matches!(
                        slice(&source, text, value_type)?,
                        "string" | "int" | "float" | "bool"
                    )
                    || opaque.iter().any(|region| {
                        value_type.start_byte() >= region.start
                            && value_type.start_byte() < region.end
                    }))
            {
                return Err(DqlQueryError);
            }
        }
        let references = bindings(&source, text, &nodes)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(text.len())
            .map_err(|_| DqlQueryError)?;
        owned.push_str(text);
        Ok(Self {
            source,
            text: owned,
            nodes,
            references,
        })
    }

    /// Returns the complete physical file identity shared by every native role.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns source-ordered native query roles and their parent indices.
    #[must_use]
    pub fn nodes(&self) -> &[DqlNode] {
        &self.nodes
    }

    /// Returns derived local variable edges; schema names and builtins are unresolved.
    #[must_use]
    pub fn references(&self) -> &[DqlReference] {
        &self.references
    }

    /// Returns the exact physical spelling of an admitted native role.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Reconstructs a node's source identity from the one authoritative owner.
    #[must_use]
    pub fn node_source(&self, index: usize) -> Option<SourceRef> {
        let span = self.nodes.get(index)?.span;
        Some(SourceRef::new(
            self.source.repository(),
            self.source.generation(),
            span,
            self.source.content_hash(),
            None,
        ))
    }

    /// Verifies immutable source bytes separately from canonical envelope integrity.
    ///
    /// # Errors
    /// Rejects source differing from the admitted physical file.
    pub fn verify_source_text(&self, text: &str) -> Result<(), DqlQueryError> {
        (text == self.text && content_hash(text.as_bytes()) == self.source.content_hash())
            .then_some(())
            .ok_or(DqlQueryError)
    }

    pub(crate) fn rebind(&self, generation: GenerationId) -> Result<Self, DqlQueryError> {
        let source = SourceRef::new(
            self.source.repository(),
            generation,
            self.source.span(),
            self.source.content_hash(),
            self.source.line_hint(),
        );
        Self::from_evidence(source, &self.text, self.nodes.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpaqueKind {
    String,
    Comment,
    Iri,
    Regex,
}

struct OpaqueRegion {
    start: u64,
    end: u64,
    kind: OpaqueKind,
}

// Locate opaque native tokens without replacing the parser's grammar proof.
// Their contents cannot supply executable names or local binding occurrences.
fn opaque_regions(text: &str) -> Result<Vec<OpaqueRegion>, DqlQueryError> {
    let bytes = text.as_bytes();
    let mut regions = Vec::new();
    let mut position = 0usize;
    while let Some(&byte) = bytes.get(position) {
        let (kind, delimiter) = match byte {
            b'#' => (OpaqueKind::Comment, 0),
            b'"' => (OpaqueKind::String, b'"'),
            b'<' => (OpaqueKind::Iri, b'>'),
            b'/' => (OpaqueKind::Regex, b'/'),
            _ => {
                position += 1;
                continue;
            }
        };
        let start = position;
        position += 1;
        if kind == OpaqueKind::Comment {
            while bytes
                .get(position)
                .is_some_and(|byte| !matches!(byte, b'\r' | b'\n'))
            {
                position += 1;
            }
        } else {
            loop {
                let byte = *bytes.get(position).ok_or(DqlQueryError)?;
                position += 1;
                if byte == delimiter {
                    break;
                }
                if kind != OpaqueKind::Iri && byte == b'\\' {
                    bytes.get(position).ok_or(DqlQueryError)?;
                    position += 1;
                }
            }
            if kind == OpaqueKind::Regex && bytes.get(position) == Some(&b'i') {
                position += 1;
            }
        }
        if regions.len() >= MAX_DQL_QUERY_NODES {
            return Err(DqlQueryError);
        }
        regions.try_reserve(1).map_err(|_| DqlQueryError)?;
        regions.push(OpaqueRegion {
            start: start.try_into().map_err(|_| DqlQueryError)?,
            end: position.try_into().map_err(|_| DqlQueryError)?,
            kind,
        });
    }
    Ok(regions)
}

fn opaque_ownership(node: &DqlNode, regions: &[OpaqueRegion]) -> Result<bool, DqlQueryError> {
    for region in regions {
        let begins_inside =
            node.span.start_byte() >= region.start && node.span.start_byte() < region.end;
        let full_iri = matches!(node.role, DqlNodeKind::Predicate { .. })
            && node.name.is_some_and(|name| {
                name.start_byte() == region.start && name.end_byte() == region.end
            })
            && region.kind == OpaqueKind::Iri;
        if begins_inside && node.role != DqlNodeKind::Operation && !full_iri {
            let literal_kind = match node.role {
                DqlNodeKind::Literal {
                    class: DqlLiteralKind::String,
                } => Some(OpaqueKind::String),
                DqlNodeKind::Literal {
                    class: DqlLiteralKind::Regex,
                } => Some(OpaqueKind::Regex),
                _ => None,
            };
            if literal_kind != Some(region.kind)
                || node.span.start_byte() != region.start
                || node.span.end_byte() != region.end
            {
                return Ok(false);
            }
        }
        if node
            .name
            .is_some_and(|name| name.start_byte() >= region.start && name.start_byte() < region.end)
            && !full_iri
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn contains(owner: SourceSpan, child: SourceSpan) -> bool {
    owner.file() == child.file()
        && owner.start_byte() <= child.start_byte()
        && owner.end_byte() >= child.end_byte()
}

fn slice<'a>(
    source: &SourceRef,
    text: &'a str,
    span: SourceSpan,
) -> Result<&'a str, DqlQueryError> {
    if !contains(source.span(), span) || span.start_byte() == span.end_byte() {
        return Err(DqlQueryError);
    }
    let start = usize::try_from(span.start_byte()).map_err(|_| DqlQueryError)?;
    let end = usize::try_from(span.end_byte()).map_err(|_| DqlQueryError)?;
    text.get(start..end).ok_or(DqlQueryError)
}

fn requires_name(role: DqlNodeKind) -> bool {
    matches!(
        role,
        DqlNodeKind::Root { .. }
            | DqlNodeKind::Predicate { .. }
            | DqlNodeKind::Call { .. }
            | DqlNodeKind::Boolean { .. }
            | DqlNodeKind::Parameter { .. }
            | DqlNodeKind::Binding
            | DqlNodeKind::Alias
    )
}

fn identifier(text: &str) -> bool {
    text.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn valid_name(role: DqlNodeKind, text: &str) -> bool {
    match role {
        DqlNodeKind::Parameter { .. } => text.strip_prefix('$').is_some_and(identifier),
        DqlNodeKind::Boolean { operator } => text.eq_ignore_ascii_case(match operator {
            DqlBooleanOperator::And => "and",
            DqlBooleanOperator::Or => "or",
            DqlBooleanOperator::Not => "not",
        }),
        DqlNodeKind::Predicate { .. } if text.starts_with('<') => text
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .is_some_and(|s| {
                !s.is_empty()
                    && !s.chars().any(|c| {
                        c.is_whitespace()
                            || matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\')
                    })
            }),
        _ => identifier(text),
    }
}

fn allowed_parent(role: DqlNodeKind, parent: Option<DqlNodeKind>) -> bool {
    matches!(
        (role, parent),
        (DqlNodeKind::Operation, None)
            | (
                DqlNodeKind::Root { .. } | DqlNodeKind::Parameter { .. },
                Some(DqlNodeKind::Operation),
            )
            | (
                DqlNodeKind::Call {
                    class: DqlCallKind::Generator,
                },
                Some(DqlNodeKind::Root { aggregation: false }),
            )
            | (
                DqlNodeKind::ParameterReference,
                Some(DqlNodeKind::Call { .. } | DqlNodeKind::List)
            )
            | (
                DqlNodeKind::VariableReference,
                Some(DqlNodeKind::Call {
                    class: DqlCallKind::Generator | DqlCallKind::Nested | DqlCallKind::Value,
                }),
            )
            | (
                DqlNodeKind::Binding | DqlNodeKind::Alias,
                Some(DqlNodeKind::Root { .. } | DqlNodeKind::Selection | DqlNodeKind::Call { .. }),
            )
            | (DqlNodeKind::Language, Some(DqlNodeKind::Predicate { .. }))
            | (
                DqlNodeKind::Selection,
                Some(DqlNodeKind::Root { .. } | DqlNodeKind::Selection)
            )
            | (
                DqlNodeKind::Predicate { .. },
                Some(DqlNodeKind::Selection | DqlNodeKind::Call { .. }),
            )
            | (
                DqlNodeKind::Filter,
                Some(DqlNodeKind::Root { .. } | DqlNodeKind::Selection)
            )
            | (
                DqlNodeKind::Boolean { .. } | DqlNodeKind::Group,
                Some(DqlNodeKind::Filter | DqlNodeKind::Boolean { .. } | DqlNodeKind::Group),
            )
            | (
                DqlNodeKind::Call { .. },
                Some(
                    DqlNodeKind::Root { .. }
                        | DqlNodeKind::Selection
                        | DqlNodeKind::Call { .. }
                        | DqlNodeKind::Filter
                        | DqlNodeKind::Boolean { .. }
                        | DqlNodeKind::Group,
                ),
            )
            | (
                DqlNodeKind::Literal { .. } | DqlNodeKind::List,
                Some(DqlNodeKind::Call { .. } | DqlNodeKind::List | DqlNodeKind::Parameter { .. }),
            )
    )
}

fn bindings(
    source: &SourceRef,
    text: &str,
    nodes: &[DqlNode],
) -> Result<Vec<DqlReference>, DqlQueryError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(nodes.len())
        .map_err(|_| DqlQueryError)?;
    for (index, node) in nodes.iter().enumerate() {
        let parameter = node.role == DqlNodeKind::ParameterReference;
        if !parameter && node.role != DqlNodeKind::VariableReference {
            continue;
        }
        let spelling = slice(source, text, node.span)?;
        let name = if parameter {
            spelling.strip_prefix('$').ok_or(DqlQueryError)?
        } else {
            spelling
        };
        if !identifier(name) {
            return Err(DqlQueryError);
        }
        let mut declaration = None;
        for (candidate, owner) in nodes.iter().enumerate() {
            if !matches!(
                (parameter, owner.role),
                (true, DqlNodeKind::Parameter { .. }) | (false, DqlNodeKind::Binding)
            ) {
                continue;
            }
            let raw = slice(source, text, owner.name.ok_or(DqlQueryError)?)?;
            if raw.trim_start_matches('$') == name {
                if declaration.is_some() {
                    return Err(DqlQueryError);
                }
                declaration = Some(u32::try_from(candidate).map_err(|_| DqlQueryError)?);
            }
        }
        result.push(DqlReference {
            node: u32::try_from(index).map_err(|_| DqlQueryError)?,
            declaration,
        });
    }
    Ok(result)
}

/// Encodes a validated native query with a source-owned canonical identity.
///
/// # Errors
/// Rejects oversized canonical payloads, encoding or allocation failure.
pub fn new_dql_query_envelope(
    provenance: FactId,
    query: &DqlQuery,
) -> Result<ExtensionEnvelope, DqlQueryError> {
    let mut payload = Payload(Vec::new());
    serde_json::to_writer(&mut payload, query).map_err(|_| DqlQueryError)?;
    let payload = String::from_utf8(payload.0).map_err(|_| DqlQueryError)?;
    let source = query.source.clone();
    let derivation = vec![FactRef::File(source.span().file())];
    let identity = serde_json::to_vec(&(
        DQL_QUERY_NAMESPACE,
        DQL_QUERY_VERSION,
        source.repository(),
        source.generation(),
        provenance,
        &source,
        &payload,
        &derivation,
    ))
    .map_err(|_| DqlQueryError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.dql_query/v1", &identity).id(),
        repository: source.repository(),
        generation: source.generation(),
        namespace: DQL_QUERY_NAMESPACE.to_owned(),
        version: DQL_QUERY_VERSION.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation,
        },
    })
}

/// Decodes native roles and rederives local targets and the entire canonical envelope.
///
/// # Errors
/// Rejects malformed/oversized payloads, forged evidence or noncanonical encoding.
pub fn decode_dql_query_envelope(envelope: &ExtensionEnvelope) -> Result<DqlQuery, DqlQueryError> {
    if envelope.namespace != DQL_QUERY_NAMESPACE
        || envelope.payload.len() > MAX_DQL_QUERY_PAYLOAD_BYTES
    {
        return Err(DqlQueryError);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        source: SourceRef,
        text: String,
        nodes: Vec<DqlNode>,
        references: Vec<DqlReference>,
    }
    let wire: Wire = serde_json::from_str(&envelope.payload).map_err(|_| DqlQueryError)?;
    let query = DqlQuery::from_evidence(wire.source, &wire.text, wire.nodes)?;
    if query.references != wire.references
        || new_dql_query_envelope(envelope.provenance, &query)? != *envelope
    {
        return Err(DqlQueryError);
    }
    Ok(query)
}

struct Payload(Vec<u8>);
impl Write for Payload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_DQL_QUERY_PAYLOAD_BYTES)
        {
            return Err(io::Error::other("native dql payload limit"));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::other("native dql allocation"))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
